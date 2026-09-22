//! #53 acceptance through real Restate ingress. No SQL adapter is bound.
#![cfg(feature = "integration")]

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admission::{ProjectionEnvelope, WorkflowEnvelope};
use ghinvite_core::InvitationLinkId;
use ghinvite_workflows::admission;
use restate_sdk::context::{ContextSideEffects, ObjectContext, RunFuture, WorkflowContext};
use restate_sdk::endpoint::{
    Endpoint, HandleOptions, HandlerOptions, ProtocolMode, ServiceOptions,
};
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use restate_sdk::service::IntoServiceDefinition;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

struct Runtime {
    client: reqwest::Client,
    ingress: String,
    admin: String,
    server: tokio::task::JoinHandle<()>,
    received: Arc<Mutex<Received>>,
    faults: Arc<admission::Faults>,
    transport: Arc<Transport>,
    offline: Arc<AtomicBool>,
    workflow_faults: Arc<ghinvite_workflows::request_lifecycle::WorkflowFaults>,
}

#[derive(Default)]
struct Transport {
    pause_workflows: AtomicBool,
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    max_body: AtomicUsize,
}

async fn serve(
    State((endpoint, transport)): State<(Endpoint, Arc<Transport>)>,
    request: Request,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    if parts.uri.path().ends_with("/InvitationRequest/run")
        && transport.pause_workflows.load(Ordering::SeqCst)
    {
        return axum::response::Response::builder()
            .status(503)
            .body(Body::empty())
            .unwrap();
    }
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    if parts.uri.path().contains("/InvitationLink/") {
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

// Stands in for the production projector under its Restate name.
struct ProjectionConsumer(Arc<Mutex<Received>>, Arc<AtomicBool>);

#[restate_sdk::object(name = "InvitationProjection")]
impl ProjectionConsumer {
    #[handler]
    async fn apply_transition(
        &self,
        ctx: ObjectContext<'_>,
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

// Stands in for the production request workflow under its Restate name.
struct RequestConsumer(Arc<Mutex<Received>>);

#[restate_sdk::workflow(name = "InvitationRequest")]
impl RequestConsumer {
    #[handler]
    async fn notification_status(
        &self,
        _: restate_sdk::context::SharedWorkflowContext<'_>,
    ) -> Result<Json<Option<admission::TerminalSignal>>, TerminalError> {
        Ok(Json(None))
    }
    #[handler]
    async fn notify(
        &self,
        _: restate_sdk::context::SharedWorkflowContext<'_>,
        _: Json<admission::TerminalSignal>,
    ) -> Result<(), TerminalError> {
        Ok(())
    }
    #[handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(envelope): Json<WorkflowEnvelope>,
    ) -> Result<Json<ghinvite_workflows::request_lifecycle::WorkflowResult>, TerminalError> {
        ctx.run(|| async {
            self.0.lock().unwrap().workflows.push(envelope.clone());
            Ok::<_, HandlerError>(Json(
                ghinvite_workflows::request_lifecycle::WorkflowResult {
                    state: envelope.request.state,
                    dispatch: None,
                },
            ))
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
        Self::start_with_workflow(false).await
    }

    async fn start_with_workflow(real_workflow: bool) -> Self {
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
        let faults = Arc::new(admission::Faults::default());
        let offline = Arc::new(AtomicBool::new(true));
        let workflow_faults =
            Arc::new(ghinvite_workflows::request_lifecycle::WorkflowFaults::default());
        let builder = admission::bind_with_faults(Endpoint::builder(), faults.clone())
            .bind(ProjectionConsumer(received.clone(), offline.clone()));
        let builder = if real_workflow {
            let builder = ghinvite_workflows::request_lifecycle::bind_with_faults(
                builder,
                workflow_faults.clone(),
            );
            // Bind the real receiver with unavailable projection prerequisites;
            // this test checks durable submission without waiting on SQL/GitHub.
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
            ghinvite_workflows::delivery::bind(
                builder,
                ghinvite_workflows::AppState::new(storage.clone(), github),
            )
        } else {
            builder.bind(
                RequestConsumer(received.clone())
                    .into_service_definition()
                    .options(ServiceOptions::new().handler(
                        "run",
                        HandlerOptions::new().workflow_retention(Duration::from_secs(2)),
                    )),
            )
        };
        let endpoint = builder.build();
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
            workflow_faults,
        }
    }

    async fn command(&self, id: &str, handler: &str, input: &Value) -> reqwest::Response {
        self.client
            .post(format!("{}/InvitationLink/{id}/{handler}", self.ingress))
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
            .json(&json!({"query": format!("SELECT id, status FROM sys_invocation WHERE target_service_key = '{id}' AND target_service_name <> 'InvitationProjection'")}))
            .send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "introspection: {status} {text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        value["rows"].as_array().expect(&text).clone()
    }
}

async fn authoritative_workflow_contract() {
    timeout(Duration::from_secs(60), async {
        let runtime = Runtime::start_with_workflow(true).await;
        runtime.offline.store(false, Ordering::SeqCst);
        runtime.workflow_faults.early_wakes.store(1, Ordering::SeqCst);
        let now = chrono::Utc::now();
        runtime.faults.set_clock(Some(
            now - chrono::Duration::days(7) + chrono::Duration::seconds(2),
        ));
        let input = creation();
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        let attempt = json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 91});
        let receipt = runtime.ok(id, "admit", &attempt).await;
        runtime.faults.set_clock(None);
        let request_id = receipt["result"]["request_id"].as_str().unwrap();
        timeout(Duration::from_secs(1), async {
            while runtime.workflow_faults.early_wakes.load(Ordering::SeqCst) != 0 { sleep(Duration::from_millis(10)).await; }
        }).await.expect("early durable wake scheduled");
        let early = runtime.ok(id, "request_status", &json!({"link_id": id, "request_id": request_id, "requester_id": 91})).await;
        assert_eq!(early["state"], "pending");
        let response = runtime
            .client
            .get(format!(
                "{}/restate/workflow/InvitationRequest/{request_id}/attach",
                runtime.ingress
            ))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["state"], "expired");
        assert_eq!(result["dispatch"], Value::Null);
        assert!(chrono::Utc::now() >= now + chrono::Duration::seconds(2));
        runtime
            .transport
            .pause_workflows
            .store(true, Ordering::SeqCst);
        let input = creation();
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        let attempt = json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 92});
        let receipt = runtime.ok(id, "admit", &attempt).await;
        let request_id = receipt["result"]["request_id"].as_str().unwrap();
        runtime.workflow_faults.interrupt_notification.store(true, Ordering::SeqCst);
        let decision = runtime
            .ok(
                id,
                "decide",
                &json!({"link_id": id,
            "request_id": request_id, "operation_id": ghinvite_core::RequestId::new(),
            "admin": input["admin"], "action": {"kind": "approve"}}),
            )
            .await;
        let signal = json!({"link_id": id, "request_id": request_id, "revision": 2,
            "decision_id": decision["request"]["decision"]["decision_id"]});
        let notify = runtime
            .client
            .post(format!(
                "{}/InvitationRequest/{request_id}/notify",
                runtime.ingress
            ))
            .json(&signal)
            .send();
        tokio::pin!(notify);
        tokio::select! {
            result = &mut notify => { assert!(result.unwrap().status().is_success()); },
            _ = async { timeout(Duration::from_secs(10), async {
                while runtime.workflow_faults.notification_attempts.load(Ordering::SeqCst) < 2 { sleep(Duration::from_millis(20)).await; }
            }).await.expect("notification replay"); } => {}
        }
        timeout(Duration::from_secs(10), async {
            while runtime.workflow_faults.notification_attempts.load(Ordering::SeqCst) < 2 { sleep(Duration::from_millis(20)).await; }
        }).await.expect("notification interrupted after resolve");
        runtime.workflow_faults.interrupt_notification.store(false, Ordering::SeqCst);
        for task in runtime.transport.tasks.lock().unwrap().drain(..) { if !task.is_finished() { task.abort(); } }
        let notified = runtime.client.post(format!("{}/InvitationRequest/{request_id}/notify", runtime.ingress)).json(&signal).send().await.unwrap();
        assert!(notified.status().is_success());
        // A different terminal fact for a resolved request is a conflict and
        // must not replace the fact the workflow was already given.
        let mut conflicting = signal.clone();
        conflicting["decision_id"] = json!(ghinvite_core::RequestId::new());
        let conflict = runtime.client.post(format!("{}/InvitationRequest/{request_id}/notify", runtime.ingress)).json(&conflicting).send().await.unwrap();
        assert_eq!(conflict.status(), 409);
        let terminal: Value = runtime.client.post(format!("{}/InvitationRequest/{request_id}/notification_status", runtime.ingress)).send().await.unwrap().json().await.unwrap();
        assert_eq!(terminal, signal, "conflicting signal replaced the terminal fact");
        runtime
            .transport
            .pause_workflows
            .store(false, Ordering::SeqCst);
        let result: Value = runtime
            .client
            .get(format!(
                "{}/restate/workflow/InvitationRequest/{request_id}/attach",
                runtime.ingress
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["state"], "approved");
        assert_eq!(
            result["dispatch"]["dispatch_id"],
            format!("dispatch/{request_id}")
        );
        assert_eq!(
            result["dispatch"]["input"]["repos"][0]["repo_full_name"],
            "acme/api"
        );
        assert_eq!(runtime.ok(id, "admit", &attempt).await, receipt);
        timeout(Duration::from_secs(15), async { loop {
            if runtime.invocations(request_id).await.is_empty() { break; }
            sleep(Duration::from_millis(100)).await;
        }}).await.expect("completed workflow cleanup");
        assert_eq!(runtime.ok(id, "admit", &attempt).await, receipt);
        assert!(runtime.invocations(request_id).await.is_empty(), "receipt replay restarted workflow");
        let retained = runtime.ok(id, "prepare_dispatch", &json!({"link_id": id, "request_id": request_id, "requester_id": 92})).await;
        assert_eq!(retained, result["dispatch"], "workflow cleanup removed handoff");
        let mut input = creation();
        input["approval_required"] = json!(false);
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        let receipt = runtime
            .ok(
                id,
                "admit",
                &json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 93}),
            )
            .await;
        let request_id = receipt["result"]["request_id"].as_str().unwrap();
        let result: Value = runtime
            .client
            .get(format!(
                "{}/restate/workflow/InvitationRequest/{request_id}/attach",
                runtime.ingress
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["state"], "approved");
        assert_eq!(
            result["dispatch"]["input"]["request"]["decision_deadline"],
            Value::Null
        );
        let handoff = runtime
            .ok(
                id,
                "prepare_dispatch",
                &json!({"link_id": id, "request_id": request_id, "requester_id": 93}),
            )
            .await;
        assert_eq!(handoff, result["dispatch"]);
        let commands = handoff["commands"].as_array().expect("retained repository commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["repo_id"], 10);
        assert_eq!(commands[0]["requester_id"], 93);
        assert!(commands[0]["invitation_id"].as_str().unwrap().parse::<ghinvite_core::GithubInvitationId>().is_ok());
        assert_eq!(
            runtime
                .ok(
                    id,
                    "prepare_dispatch",
                    &json!({"link_id": id, "request_id": request_id, "requester_id": 93})
                )
                .await,
            handoff
        );
        interrupted_timer_setup(&runtime).await;
        waiting_notification_and_timer_races(&runtime).await;
    })
    .await
    .expect("workflow timer completion");
}

async fn interrupted_timer_setup(runtime: &Runtime) {
    let deadline = chrono::Utc::now() + chrono::Duration::seconds(3);
    runtime
        .faults
        .set_clock(Some(deadline - chrono::Duration::days(7)));
    runtime
        .workflow_faults
        .interrupt_timer
        .store(true, Ordering::SeqCst);
    let input = creation();
    let id = input["link_id"].as_str().unwrap();
    runtime.ok(id, "create", &input).await;
    let receipt = runtime
        .ok(
            id,
            "admit",
            &json!({"link_id": id,
        "operation_id": ghinvite_core::RequestId::new(), "requester_id": 94}),
        )
        .await;
    timeout(Duration::from_secs(2), async {
        while runtime
            .workflow_faults
            .timer_attempts
            .load(Ordering::SeqCst)
            < 2
        {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("sleep setup boundary reached");
    runtime.faults.set_clock(None);
    while chrono::Utc::now() < deadline {
        sleep(Duration::from_millis(20)).await;
    }
    runtime
        .workflow_faults
        .interrupt_timer
        .store(false, Ordering::SeqCst);
    for task in runtime.transport.tasks.lock().unwrap().drain(..) {
        if !task.is_finished() {
            task.abort();
        }
    }
    let request_id = receipt["result"]["request_id"].as_str().unwrap();
    let response = timeout(
        Duration::from_secs(2),
        runtime
            .client
            .get(format!(
                "{}/restate/workflow/InvitationRequest/{request_id}/attach",
                runtime.ingress
            ))
            .send(),
    )
    .await
    .expect("recovery must not restart the original relative wait")
    .unwrap();
    assert_eq!(response.json::<Value>().await.unwrap()["state"], "expired");
}

async fn waiting_notification_and_timer_races(runtime: &Runtime) {
    for late in [false, true] {
        let deadline = chrono::Utc::now() + chrono::Duration::seconds(2);
        runtime
            .faults
            .set_clock(Some(deadline - chrono::Duration::days(7)));
        let waits = runtime.workflow_faults.waits.load(Ordering::SeqCst);
        let input = creation();
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        let receipt = runtime
            .ok(
                id,
                "admit",
                &json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 95}),
            )
            .await;
        runtime.faults.set_clock(None);
        timeout(Duration::from_secs(1), async {
            while runtime.workflow_faults.waits.load(Ordering::SeqCst) == waits {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("production workflow entered wait");
        if late {
            while chrono::Utc::now() < deadline {
                sleep(Duration::from_millis(5)).await;
            }
        }
        let request_id = receipt["result"]["request_id"].as_str().unwrap();
        let decision = runtime
            .ok(
                id,
                "decide",
                &json!({"link_id": id,
            "request_id": request_id, "operation_id": ghinvite_core::RequestId::new(),
            "admin": input["admin"], "action": {"kind": "approve"}}),
            )
            .await;
        let result = timeout(
            Duration::from_secs(1),
            runtime
                .client
                .get(format!(
                    "{}/restate/workflow/InvitationRequest/{request_id}/attach",
                    runtime.ingress
                ))
                .send(),
        )
        .await
        .expect("waiting workflow did not consume terminal signal")
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
        assert_eq!(result["state"], if late { "expired" } else { "approved" });
        assert_eq!(
            decision["outcome"],
            if late { "incompatible" } else { "applied" }
        );
        if !late {
            assert!(
                chrono::Utc::now() < deadline,
                "notification only completed on timeout"
            );
        }
    }
}

/// A browser admission command, normalized as the invitation form submits it.
fn admit_command(
    link_id: InvitationLinkId,
    operation_id: &str,
    requester_id: u64,
    justification: Option<String>,
) -> ghinvite_core::admission::Admit {
    let mut command = ghinvite_core::admission::Admit {
        link_id,
        operation_id: operation_id.to_owned().try_into().unwrap(),
        requester_id,
        justification,
    };
    command.normalize().unwrap();
    command
}

fn creation() -> Value {
    json!({
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
        // Browser command facade resolves fresh links without SQL and recovers
        // normalized input after navigation, even while projection is offline.
        use ghinvite_web::{AuthorityError, LinkAuthority};
        let browser = LinkAuthority::new(Arc::new(
            ghinvite_web::RestateClient::new(&runtime.ingress).unwrap(),
        ));
        let code = created["invitation_code"].as_str().unwrap();
        let page = browser.requester_page(code, 501, None).await.unwrap();
        assert_eq!(page.link_id.to_string(), id);
        assert!(page.attempt.is_none());
        assert!(page.can_start_fresh);
        let operation = ghinvite_core::RequestId::new().to_string();
        let command = admit_command(page.link_id, &operation, 501, Some("  access  ".into()));
        browser.prepare(command.clone()).await.unwrap();
        let recovered = browser
            .requester_page(code, 501, None)
            .await
            .unwrap()
            .attempt
            .unwrap();
        assert_eq!(recovered.input.justification.as_deref(), Some("access"));
        assert!(recovered.receipt.is_none());
        assert!(
            browser
                .requester_page(code, 502, Some(operation.clone().try_into().unwrap()))
                .await
                .is_err()
        );
        let metadata_input = creation();
        let metadata_link = browser
            .create(serde_json::from_value(metadata_input.clone()).unwrap())
            .await
            .unwrap();
        let metadata = ghinvite_core::admission::UpdateMetadata {
            link_id: metadata_link.link_id,
            admin: serde_json::from_value(input["admin"].clone()).unwrap(),
            description: "Updated workshop".into(),
            internal_note: Some("Still private".into()),
        };
        let updated = browser.update_metadata(metadata).await.unwrap();
        assert_eq!(
            updated.metadata.as_ref().unwrap().description,
            "Updated workshop"
        );
        assert_eq!(updated.creation.description, "Workshop");
        let retry_input = creation();
        let retry_link = browser
            .create(serde_json::from_value(retry_input.clone()).unwrap())
            .await
            .unwrap();
        let retry_operation = ghinvite_core::RequestId::new().to_string();
        let retry = admit_command(retry_link.link_id, &retry_operation, 601, None);
        browser.prepare(retry.clone()).await.unwrap();
        // Discard the first acknowledgement, then navigate back after revoke.
        let accepted = browser.admit(retry.clone()).await.unwrap();
        assert!(
            !browser
                .requester_page(&retry_link.invitation_code, 601, None)
                .await
                .unwrap()
                .can_start_fresh
        );
        browser
            .revoke(ghinvite_core::admission::AdminLinkCommand {
                link_id: retry_link.link_id,
                admin: retry_link.creation.admin.clone(),
            })
            .await
            .unwrap();
        let mut normalized_retry = retry.clone();
        normalized_retry.justification = Some(" \t\n ".into());
        assert_eq!(browser.admit(normalized_retry).await.unwrap(), accepted);
        let recovered = browser
            .requester_page(&retry_link.invitation_code, 601, None)
            .await
            .unwrap();
        assert!(!recovered.can_start_fresh);
        assert_eq!(recovered.attempt.unwrap().receipt.unwrap(), accepted);
        assert!(matches!(
            browser
                .requester_page(&retry_link.invitation_code, 602, None)
                .await,
            Err(AuthorityError::Missing)
        ));
        let changed = admit_command(
            retry_link.link_id,
            &retry_operation,
            601,
            Some("edited".into()),
        );
        assert!(matches!(
            browser.admit(changed).await,
            Err(AuthorityError::Conflict)
        ));
        let foreign = admit_command(retry_link.link_id, &retry_operation, 602, None);
        assert!(matches!(
            browser.admit(foreign).await,
            Err(AuthorityError::Conflict)
        ));
        let second_tab = admit_command(
            retry_link.link_id,
            &ghinvite_core::RequestId::new().to_string(),
            601,
            None,
        );
        assert!(matches!(
            browser.admit(second_tab).await.unwrap().result,
            ghinvite_core::admission::AdmissionResult::Rejected {
                reason: ghinvite_core::admission::Rejection::Revoked
            }
        ));
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
        let admission = json!({"link_id": id, "operation_id": operation,
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
        for field in ["requester_id", "justification"] {
            let mut changed = admission.clone();
            changed[field] = match field {
                "requester_id" => json!(12),
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
                    received
                        .projections
                        .iter()
                        .filter(|p| p.link.link_id.to_string() == id)
                        .count()
                        == 3
                        && received
                            .workflows
                            .iter()
                            .filter(|w| w.request.link_id.to_string() == id)
                            .count()
                            == 1
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
            let workflow = received
                .workflows
                .iter()
                .find(|w| w.request.link_id.to_string() == id)
                .unwrap();
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
                .filter(|p| p.link.link_id.to_string() == id)
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
        lifecycle_boundaries(&runtime).await;
        overdue_status_and_timer_race(&runtime).await;
        overdue_readmission(&runtime).await;
        lifecycle_recovery(&runtime).await;
        interruption_recovery(&runtime).await;
        queued_expiry(&runtime).await;
        seeded_terminal_requests(&runtime).await;
        retention_and_bounded_history(&runtime).await;
    })
    .await
    .expect("authoritative admission acceptance exceeded 120 seconds");
    authoritative_workflow_contract().await;
}

async fn lifecycle_boundaries(runtime: &Runtime) {
    let now = chrono::Utc::now();
    for (offset, action, expected) in [
        (-1, "approve", "approved"),
        (-1, "decline", "declined"),
        (0, "approve", "expired"),
        (1, "decline", "expired"),
    ] {
        runtime.faults.set_clock(Some(now));
        let mut input = creation();
        input["max_uses"] = json!(3);
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        let attempt = json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 81});
        let admitted = runtime.ok(id, "admit", &attempt).await;
        let deadline = now + chrono::Duration::days(7);
        runtime
            .faults
            .set_clock(Some(deadline + chrono::Duration::milliseconds(offset)));
        let decision = json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(),
            "request_id": admitted["result"]["request_id"], "admin": input["admin"],
            "action": {"kind": action}});
        assert_eq!(
            runtime.ok(id, "decision_status", &decision).await,
            Value::Null
        );
        let result = runtime.ok(id, "decide", &decision).await;
        assert_eq!(result["request"]["state"], expected);
        assert_eq!(
            result["outcome"],
            if expected == "expired" {
                "incompatible"
            } else {
                "applied"
            }
        );
        runtime
            .faults
            .set_clock(Some(deadline + chrono::Duration::days(1)));
        assert_eq!(runtime.ok(id, "decide", &decision).await, result);
        assert_eq!(runtime.ok(id, "decision_status", &decision).await, result);
        for field in ["admin", "request_id"] {
            let mut foreign = decision.clone();
            foreign[field] = match field {
                "admin" => json!({"account_id": 200, "user_id": 7}),
                "request_id" => json!(ghinvite_core::RequestId::new()),
                _ => unreachable!(),
            };
            assert_eq!(
                runtime.command(id, "decide", &foreign).await.status(),
                if field == "admin" { 404 } else { 409 }
            );
            assert_eq!(
                runtime
                    .command(id, "decision_status", &foreign)
                    .await
                    .status(),
                if field == "admin" { 404 } else { 409 }
            );
        }
        let mut changed = decision.clone();
        changed["action"] =
            json!({"kind": if action == "approve" { "decline" } else { "approve" }});
        assert_eq!(runtime.command(id, "decide", &changed).await.status(), 409);
        changed["operation_id"] = json!(ghinvite_core::RequestId::new());
        assert_eq!(
            runtime.ok(id, "decide", &changed).await["outcome"],
            "incompatible"
        );
        let mut matching = decision.clone();
        matching["operation_id"] = json!(ghinvite_core::RequestId::new());
        assert_eq!(
            runtime.ok(id, "decide", &matching).await["outcome"],
            if expected == "expired" {
                "incompatible"
            } else {
                "already_completed"
            }
        );
        let query = json!({"link_id": id, "request_id": admitted["result"]["request_id"], "requester_id": 81});
        assert_eq!(
            runtime.ok(id, "request_status", &query).await["state"],
            expected
        );
        assert_eq!(runtime.ok(id, "admit", &attempt).await, admitted);
        let mut fresh = attempt.clone();
        fresh["operation_id"] = json!(ghinvite_core::RequestId::new());
        assert_eq!(
            runtime.ok(id, "admit", &fresh).await["result"]["kind"],
            if expected == "approved" {
                "rejected"
            } else {
                "accepted"
            }
        );
    }
    runtime.faults.set_clock(None);
}

async fn overdue_status_and_timer_race(runtime: &Runtime) {
    for status_first in [false, true] {
        let now = chrono::Utc::now();
        runtime.faults.set_clock(Some(now));
        let input = creation();
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        let receipt = runtime
            .ok(
                id,
                "admit",
                &json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 85}),
            )
            .await;
        let query = json!({"link_id": id, "request_id": receipt["result"]["request_id"], "requester_id": 85});
        runtime
            .faults
            .set_clock(Some(now + chrono::Duration::days(7)));
        let command = json!({"link_id": id, "request_id": receipt["result"]["request_id"],
            "operation_id": ghinvite_core::RequestId::new(), "admin": input["admin"], "action": {"kind": "approve"}});
        let (status, decision) = if status_first {
            tokio::join!(
                runtime.ok(id, "request_status", &query),
                runtime.ok(id, "decide", &command)
            )
        } else {
            let (decision, status) = tokio::join!(
                runtime.ok(id, "decide", &command),
                runtime.ok(id, "request_status", &query)
            );
            (status, decision)
        };
        assert_eq!(status["state"], "expired");
        assert_eq!(decision["outcome"], "incompatible");
        assert_eq!(
            status["decision"]["effective_at"],
            receipt["result"]["decision_deadline"]
        );
    }
    runtime.faults.set_clock(None);
}

async fn lifecycle_recovery(runtime: &Runtime) {
    for (handler, stages) in [
        (
            "decide",
            vec![
                "lifecycle-before-decision",
                "lifecycle-after-decision",
                "lifecycle-after-request",
                "lifecycle-after-blocker",
                "lifecycle-after-projection-send",
                "lifecycle-after-notification-send",
                "lifecycle-after-outcome",
            ],
        ),
        (
            "admit",
            vec![
                "after-decision",
                "after-old-request",
                "after-old-blocker",
                "after-request",
                "after-outcome",
                "after-projection-send",
                "after-old-notification-send",
                "after-workflow-send",
            ],
        ),
    ] {
        for stage in stages {
            let now = chrono::Utc::now();
            runtime.faults.set_clock(Some(now));
            let mut input = creation();
            input["max_uses"] = json!(3);
            let id = input["link_id"].as_str().unwrap();
            runtime.ok(id, "create", &input).await;
            let attempt = json!({"link_id": id,
                "operation_id": ghinvite_core::RequestId::new(), "requester_id": 83});
            let original = runtime.ok(id, "admit", &attempt).await;
            let deadline = now + chrono::Duration::days(7);
            runtime.faults.set_clock(Some(
                deadline + chrono::Duration::milliseconds(if handler == "decide" { -1 } else { 0 }),
            ));
            let command = if handler == "decide" {
                json!({"link_id": id, "request_id": original["result"]["request_id"],
                    "operation_id": ghinvite_core::RequestId::new(), "admin": input["admin"], "action": {"kind": "approve"}})
            } else {
                json!({"operation_id": ghinvite_core::RequestId::new(), "link_id": id, "requester_id": 83})
            };
            runtime.faults.arm(stage);
            let call = runtime.ok(id, handler, &command);
            tokio::pin!(call);
            tokio::select! {
                _ = &mut call => panic!("completed before {stage}"),
                _ = async { timeout(Duration::from_secs(10), async {
                    while runtime.faults.attempts() < 2 { sleep(Duration::from_millis(20)).await; }
                }).await.expect("checkpoint readiness"); } => {}
            }
            let query = json!({"link_id": id, "request_id": original["result"]["request_id"], "requester_id": 83});
            let read = runtime.ok(id, "request_status", &query);
            tokio::pin!(read);
            assert!(
                timeout(Duration::from_millis(50), &mut read).await.is_err(),
                "partial read at {stage}"
            );
            runtime
                .faults
                .set_clock(Some(deadline + chrono::Duration::seconds(1)));
            runtime.faults.release();
            for task in runtime.transport.tasks.lock().unwrap().drain(..) {
                if !task.is_finished() {
                    task.abort();
                }
            }
            let result = call.await;
            let expected = if handler == "decide" && stage != "lifecycle-before-decision" {
                "approved"
            } else {
                "expired"
            };
            assert_eq!(read.await["state"], expected, "{stage}");
            assert_eq!(runtime.ok(id, handler, &command).await, result, "{stage}");
            assert_eq!(runtime.ok(id, "admit", &attempt).await, original);
            let new_attempt = json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 83});
            if handler == "admit" || expected == "approved" {
                assert_eq!(
                    runtime.ok(id, "admit", &new_attempt).await["result"]["reason"],
                    "existing_request",
                    "{stage}"
                );
            }
            timeout(Duration::from_secs(10), async {
                loop {
                    let envelopes = runtime.received.lock().unwrap().projections.clone();
                    let events: Vec<_> = envelopes
                        .iter()
                        .filter(|e| e.link.link_id.to_string() == id)
                        .flat_map(|e| &e.events)
                        .filter(|e| e.kind.as_str() == format!("request.{expected}"))
                        .collect();
                    if !events.is_empty() {
                        assert_eq!(events.len(), 1, "{stage}");
                        break;
                    }
                    sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("terminal audit not dispatched");
            assert_eq!(
                runtime
                    .ok(
                        id,
                        "link_status",
                        &json!({"link_id": id, "admin": input["admin"]})
                    )
                    .await["uses"],
                if handler == "admit" { 2 } else { 1 }
            );
        }
    }
    runtime.faults.set_clock(None);
}

async fn overdue_readmission(runtime: &Runtime) {
    for rejection in [None, Some("exhausted"), Some("revoked"), Some("expired")] {
        let now = chrono::Utc::now();
        runtime.faults.set_clock(Some(now));
        let mut input = creation();
        input["max_uses"] = json!(if rejection == Some("exhausted") { 1 } else { 3 });
        if rejection == Some("expired") {
            input["expires_at"] = json!(now + chrono::Duration::days(1));
        }
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        let attempt = json!({"link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 82});
        let admitted = runtime.ok(id, "admit", &attempt).await;
        if rejection == Some("revoked") {
            runtime
                .ok(
                    id,
                    "revoke",
                    &json!({"link_id": id, "admin": input["admin"]}),
                )
                .await;
        }
        runtime
            .faults
            .set_clock(Some(now + chrono::Duration::days(8)));
        let before = runtime.received.lock().unwrap().projections.len();
        assert_eq!(runtime.ok(id, "admit", &attempt).await, admitted);
        let mut fresh = attempt.clone();
        fresh["operation_id"] = json!(ghinvite_core::RequestId::new());
        let result = runtime.ok(id, "admit", &fresh).await;
        assert_eq!(
            result["result"]["reason"],
            rejection.map_or(Value::Null, |s| json!(s))
        );
        timeout(Duration::from_secs(10), async {
            loop {
                let envelopes = runtime.received.lock().unwrap().projections.clone();
                if let Some(envelope) = envelopes.iter().find(|e| {
                    e.link.link_id.to_string() == id
                        && e.events
                            .iter()
                            .any(|e| e.kind.as_str() == "request.expired")
                }) {
                    assert_eq!(
                        envelope.requests.len(),
                        if rejection.is_none() { 2 } else { 1 }
                    );
                    assert_eq!(envelope.link.uses, if rejection.is_none() { 2 } else { 1 });
                    let expired = envelope
                        .requests
                        .iter()
                        .find(|r| r.state == ghinvite_core::RequestState::Expired)
                        .unwrap();
                    assert_eq!(
                        expired.request_id.to_string(),
                        admitted["result"]["request_id"]
                    );
                    assert_eq!(
                        expired.decision.as_ref().unwrap().effective_at,
                        now + chrono::Duration::days(7)
                    );
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("fresh admission must project overdue expiry, even on rejection");
        let old = json!({"link_id": id, "request_id": admitted["result"]["request_id"], "requester_id": 82});
        assert_eq!(
            runtime.ok(id, "request_status", &old).await["state"],
            "expired"
        );
        if rejection.is_none() {
            fresh["operation_id"] = json!(ghinvite_core::RequestId::new());
            assert_eq!(
                runtime.ok(id, "admit", &fresh).await["result"]["reason"],
                "existing_request"
            );
        }
        assert!(runtime.received.lock().unwrap().projections.len() > before);
    }
    runtime.faults.set_clock(None);
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
    let first =
        json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 61});
    let second =
        json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 62});
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
        let attempt = json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 41});
        let client = runtime.client.clone();
        let url = format!("{}/InvitationLink/{id}/admit", runtime.ingress);
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

async fn seeded_terminal_requests(runtime: &Runtime) {
    // Seed historical state through the Restate admin state API on new fixture
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
            ("link".to_owned(), link),
            ("creation".to_owned(), original_link),
            (format!("request/{request_id}"), old_request.clone()),
            ("blocker/71".to_owned(), json!(request_id)),
        ] {
            state.insert(key, json!(serde_json::to_vec(&value).unwrap()));
        }
        let response = runtime
            .client
            .post(format!("{}/services/InvitationLink/state", runtime.admin))
            .json(&json!({"object_key": id, "new_state": state}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            202,
            "fixture state seed: {}",
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
        .expect("historical fixture state not visible");
        let attempt = json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 71});
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
    let attempt =
        json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 51});
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
        let rejected = json!({"link_id": id,
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
    let mut attempt = json!({"link_id": id,
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
    let a =
        json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 31});
    let b =
        json!({"link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 32});
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
