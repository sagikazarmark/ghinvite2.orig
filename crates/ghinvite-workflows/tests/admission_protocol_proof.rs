//! Throwaway #48 protocol proof, not production admission handlers.
//! Run: bash scripts/test-restate.sh admission_protocol_proof
//! Real Restate + SDK request-response mode + isolated SQLx SQLite projection.
//! Deliberately small aggregate/schema; this does not prove D1 adapter conformance.

#![cfg(feature = "integration")]

#[path = "admission_protocol_proof/split_state.rs"]
mod split_state;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use chrono::Utc;
use restate_sdk::context::{
    Context, ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
    RunFuture, RunRetryPolicy, SharedObjectContext, WorkflowContext,
};
use restate_sdk::endpoint::{Endpoint, HandleOptions, ProtocolMode};
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep, timeout};

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
struct LinkState {
    revision: u64,
    uses: u32,
    revoked: bool,
    expires_at_ms: i64,
    outcomes: BTreeMap<String, Outcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
struct Admission {
    operation: String,
    requester: String,
    justification: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
struct Outcome {
    input: Admission,
    result: String,
    decided_at_ms: i64,
    deadline_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Projection {
    link: String,
    state: LinkState,
    events: Vec<String>,
}

// Test-only controls. No authoritative business state lives outside Restate.
#[derive(Default)]
struct Faults {
    stage: Mutex<Option<String>>,
    attempts: AtomicUsize,
    aborted: AtomicUsize,
    offline: AtomicBool,
    projection_failures: AtomicUsize,
    lose_ack: AtomicBool,
    lost_acks: AtomicUsize,
    workflow_starts: AtomicUsize,
    tasks: Mutex<Vec<AbortHandle>>,
    history_leaked: AtomicBool,
    split_request_bytes: AtomicUsize,
}

impl Faults {
    fn arm(&self, stage: &str) {
        self.attempts.store(0, Ordering::SeqCst);
        *self.stage.lock().unwrap() = Some(stage.into());
    }

    async fn checkpoint(&self, stage: &str) -> Result<(), HandlerError> {
        let active = self.stage.lock().unwrap().as_deref() == Some(stage);
        if active {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                // Return an SDK retryable error first, allowing the earlier
                // request-response journal prefix to reach Restate durably.
                return Err(std::io::Error::other("proof: interruption").into());
            }
            // On the replay, the test aborts the SDK task here, dropping the
            // invocation body/future, then permits a fresh runtime invocation.
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

fn retry_policy() -> RunRetryPolicy {
    RunRetryPolicy::default()
        .initial_delay(Duration::from_millis(100))
        .max_delay(Duration::from_millis(200))
}

#[restate_sdk::object]
trait AdmissionProofLink {
    async fn initialize(expires_at_ms: i64) -> Result<(), TerminalError>;
    async fn admit(input: Json<Admission>) -> Result<Json<Outcome>, TerminalError>;
    async fn revoke() -> Result<(), TerminalError>;
    #[shared]
    async fn status() -> Result<Json<LinkState>, TerminalError>;
}

struct LinkHandler(Arc<Faults>);

impl AdmissionProofLink for LinkHandler {
    async fn initialize(
        &self,
        ctx: ObjectContext<'_>,
        expires_at_ms: i64,
    ) -> Result<(), TerminalError> {
        assert!(ctx.get::<Json<LinkState>>("state").await?.is_none());
        ctx.set(
            "state",
            Json(LinkState {
                expires_at_ms,
                ..Default::default()
            }),
        );
        Ok(())
    }

    async fn status(&self, ctx: SharedObjectContext<'_>) -> Result<Json<LinkState>, TerminalError> {
        Ok(ctx.get("state").await?.expect("initialized proof link"))
    }

    async fn admit(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Admission>,
    ) -> Result<Json<Outcome>, TerminalError> {
        let Json(before) = ctx.get::<Json<LinkState>>("state").await?.unwrap();
        if let Some(previous) = before.outcomes.get(&input.operation) {
            if previous.input != input {
                return Err(TerminalError::new_with_code(
                    409,
                    "operation payload conflict",
                ));
            }
            return Ok(Json(previous.clone()));
        }

        ctx.run(|| self.0.checkpoint("before-decision"))
            .name("before-decision")
            .retry_policy(retry_policy())
            .await?;

        // Journal the WHOLE decision using an immutable, exclusively read
        // snapshot. The clock is sampled inside the same run as the decision.
        // There are no external reads or context calls inside this closure.
        let Json(after) = ctx
            .run(|| async {
                let mut after = before.clone();
                let now = Utc::now().timestamp_millis();
                let result = if after.revoked {
                    "revoked"
                } else if now >= after.expires_at_ms {
                    "expired"
                } else if after.uses == 1 {
                    "exhausted"
                } else {
                    "accepted"
                };
                if result == "accepted" {
                    after.uses += 1;
                }
                after.revision += 1;
                after.outcomes.insert(
                    input.operation.clone(),
                    Outcome {
                        input: input.clone(),
                        result: result.into(),
                        decided_at_ms: now,
                        deadline_ms: (result == "accepted")
                            .then_some(now + 7 * 24 * 60 * 60 * 1000),
                    },
                );
                Ok::<_, HandlerError>(Json(after))
            })
            .name("decide_admission")
            .await?;

        ctx.run(|| self.0.checkpoint("after-decision"))
            .name("after-decision")
            .retry_policy(retry_policy())
            .await?;
        // One coherent state value for this bounded proof. Production must
        // address state size; this is not a scalable operation-ledger layout.
        ctx.set("state", Json(after.clone()));
        ctx.run(|| self.0.checkpoint("after-state"))
            .name("after-state")
            .retry_policy(retry_policy())
            .await?;
        let outcome = after.outcomes[&input.operation].clone();
        let events = if outcome.result == "accepted" {
            vec![format!("request:{}", input.operation), "exhausted".into()]
        } else {
            vec![]
        };
        let handle = ctx
            .service_client::<AdmissionProofProjectionClient>()
            .apply(Json(Projection {
                link: ctx.key().into(),
                state: after,
                events,
            }))
            .send();
        // This awaits the durable send identity, not projector completion.
        use restate_sdk::context::InvocationHandle;
        handle.invocation_id().await?;
        ctx.run(|| self.0.checkpoint("after-projection-send"))
            .name("after-projection-send")
            .retry_policy(retry_policy())
            .await?;
        if outcome.result == "accepted" {
            ctx.workflow_client::<AdmissionProofRequestClient>(format!(
                "{}-{}",
                ctx.key(),
                input.operation
            ))
            .run(Json(outcome.clone()))
            .send()
            .invocation_id()
            .await?;
        }
        ctx.run(|| self.0.checkpoint("after-workflow-send"))
            .name("after-workflow-send")
            .retry_policy(retry_policy())
            .await?;
        Ok(Json(outcome))
    }

    async fn revoke(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        let Json(mut state) = ctx.get::<Json<LinkState>>("state").await?.unwrap();
        if state.revoked {
            return Ok(());
        }
        state.revoked = true;
        state.revision += 1;
        ctx.set("state", Json(state.clone()));
        use restate_sdk::context::InvocationHandle;
        ctx.service_client::<AdmissionProofProjectionClient>()
            .apply(Json(Projection {
                link: ctx.key().into(),
                state,
                events: vec!["revoked".into()],
            }))
            .send()
            .invocation_id()
            .await?;
        Ok(())
    }
}

#[restate_sdk::workflow]
trait AdmissionProofRequest {
    async fn run(outcome: Json<Outcome>) -> Result<(), TerminalError>;
}

struct RequestHandler(Arc<Faults>);

impl AdmissionProofRequest for RequestHandler {
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(outcome): Json<Outcome>,
    ) -> Result<(), TerminalError> {
        assert_eq!(outcome.result, "accepted");
        ctx.run(|| async {
            self.0.workflow_starts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HandlerError>(())
        })
        .name("observe_start")
        .await?;
        // No database reads. This observes startup, not the production lifecycle.
        Ok(())
    }
}

#[restate_sdk::service]
trait AdmissionProofProjection {
    async fn apply(projection: Json<Projection>) -> Result<(), TerminalError>;
}

struct ProjectionHandler {
    pool: SqlitePool,
    faults: Arc<Faults>,
}

impl AdmissionProofProjection for ProjectionHandler {
    async fn apply(
        &self,
        ctx: Context<'_>,
        Json(projection): Json<Projection>,
    ) -> Result<(), TerminalError> {
        ctx.run(|| async {
            let mut transaction = self.pool.begin().await?;
            // The test installs a real failing SQLite trigger during outage.
            // Increment before SQL so we know the projector actually attempted.
            if self.faults.offline.load(Ordering::SeqCst) {
                self.faults.projection_failures.fetch_add(1, Ordering::SeqCst);
            }
            sqlx::query("INSERT INTO proof_links VALUES (?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision, uses=excluded.uses, revoked=excluded.revoked WHERE excluded.revision > proof_links.revision")
                .bind(&projection.link).bind(projection.state.revision as i64)
                .bind(projection.state.uses).bind(projection.state.revoked)
                .execute(&mut *transaction).await?;
            for outcome in projection.state.outcomes.values().filter(|o| o.result == "accepted") {
                sqlx::query("INSERT INTO proof_requests VALUES (?1, ?2, ?3) ON CONFLICT(link, operation) DO NOTHING")
                    .bind(&projection.link).bind(&outcome.input.operation).bind(outcome.decided_at_ms)
                    .execute(&mut *transaction).await?;
            }
            // Historical events must survive even when the snapshot is stale.
            for event in &projection.events {
                sqlx::query("INSERT INTO proof_events VALUES (?1, ?2) ON CONFLICT(link, event) DO NOTHING")
                    .bind(&projection.link).bind(event).execute(&mut *transaction).await?;
            }
            transaction.commit().await?;
            if self.faults.lose_ack.swap(false, Ordering::SeqCst) {
                self.faults.lost_acks.fetch_add(1, Ordering::SeqCst);
                return Err(std::io::Error::other("proof: commit acknowledgement lost").into());
            }
            Ok::<_, HandlerError>(())
        }).name("project").retry_policy(retry_policy()).await
    }
}

#[derive(Clone)]
struct Server {
    endpoint: Endpoint,
    faults: Arc<Faults>,
}

async fn serve(State(server): State<Server>, request: Request) -> axum::response::Response {
    // Buffer input like the Worker and advertise the same protocol mode.
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    if parts.uri.path().contains("/AdmissionSplitProof/") {
        server
            .faults
            .split_request_bytes
            .fetch_max(bytes.len(), Ordering::SeqCst);
        if bytes
            .windows(b"UNRELATED_HISTORY_PAYLOAD".len())
            .any(|window| window == b"UNRELATED_HISTORY_PAYLOAD")
        {
            server.faults.history_leaked.store(true, Ordering::SeqCst);
        }
    }
    let task = tokio::spawn(async move {
        let response = server.endpoint.handle_with_options(
            Request::from_parts(parts, Body::from(bytes)),
            HandleOptions {
                protocol_mode: ProtocolMode::RequestResponse,
            },
        );
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(Body::new(body), 1024 * 1024).await.unwrap();
        axum::response::Response::from_parts(parts, Body::from(bytes))
    });
    server
        .faults
        .tasks
        .lock()
        .unwrap()
        .push(task.abort_handle());
    task.await.unwrap_or_else(|_| {
        axum::response::Response::builder()
            .status(503)
            .body(Body::from("proof: endpoint task aborted"))
            .unwrap()
    })
}

async fn until(description: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out: {description}");
        sleep(Duration::from_millis(25)).await;
    }
}

async fn post(client: &reqwest::Client, url: &str, input: &impl Serialize) -> reqwest::Response {
    let value = serde_json::to_value(input).unwrap();
    let request = client.post(url);
    let request = if value.is_null() {
        request
    } else {
        request.json(&value)
    };
    let response = request.send().await.unwrap();
    assert!(
        response.status().is_success(),
        "proof request: HTTP {}",
        response.status()
    );
    response
}

#[tokio::test]
async fn restate_authoritative_admission_survives_interruption_and_projection_outage() {
    timeout(Duration::from_secs(120), scenario())
        .await
        .expect("proof exceeded 120 seconds");
}

async fn scenario() {
    let env = |key| {
        std::env::var(key).expect("run bash scripts/test-restate.sh admission_protocol_proof")
    };
    let admin = env("RESTATE_ADMIN_URL");
    let ingress = env("RESTATE_INGRESS_URL");
    let host = env("RESTATE_ENDPOINT_HOST");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    for _ in 0..100 {
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
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE proof_links(id TEXT PRIMARY KEY, revision INTEGER, uses INTEGER, revoked INTEGER);
        CREATE TABLE proof_requests(link TEXT REFERENCES proof_links(id), operation TEXT, admitted_at INTEGER, PRIMARY KEY(link, operation));
        CREATE TABLE proof_events(link TEXT REFERENCES proof_links(id), event TEXT, PRIMARY KEY(link, event));
        CREATE TRIGGER proof_offline BEFORE INSERT ON proof_links BEGIN SELECT RAISE(FAIL, 'projection unavailable'); END;")
        .execute(&pool).await.unwrap();
    let faults = Arc::new(Faults::default());
    faults.offline.store(true, Ordering::SeqCst);
    let endpoint = Endpoint::builder()
        .bind(LinkHandler(faults.clone()).serve())
        .bind(RequestHandler(faults.clone()).serve())
        .bind(
            ProjectionHandler {
                pool: pool.clone(),
                faults: faults.clone(),
            }
            .serve(),
        );
    let endpoint = split_state::bind(endpoint, faults.clone()).build();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().fallback(serve).with_state(Server {
        endpoint,
        faults: faults.clone(),
    });
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    post(
        &client,
        &format!("{admin}/deployments"),
        &serde_json::json!({"uri": format!("http://{host}:{port}")}),
    )
    .await;

    let mut accepted_states = Vec::new();
    for stage in [
        "after-decision",
        "after-state",
        "after-projection-send",
        "after-workflow-send",
    ] {
        let link = format!("{}-{stage}", ghinvite_core::RequestId::new());
        let base = format!("{ingress}/AdmissionProofLink/{link}");
        let expires_at = Utc::now().timestamp_millis()
            + if stage == "after-decision" {
                2_000
            } else {
                60_000
            };
        post(&client, &format!("{base}/initialize"), &expires_at).await;
        faults.arm(stage);
        let input = Admission {
            operation: "winner".into(),
            requester: "alice".into(),
            justification: "access".into(),
        };
        // Start a real ingress request, then kill the SDK execution on replay.
        let call = tokio::spawn({
            let client = client.clone();
            let url = format!("{base}/admit");
            let input = input.clone();
            async move {
                post(&client, &url, &input)
                    .await
                    .json::<Outcome>()
                    .await
                    .unwrap()
            }
        });
        until("checkpoint replay reached", || {
            faults.attempts.load(Ordering::SeqCst) >= 2
        })
        .await;
        if stage == "after-decision" {
            while Utc::now().timestamp_millis() <= expires_at {
                sleep(Duration::from_millis(25)).await;
            }
        }
        *faults.stage.lock().unwrap() = None;
        for handle in faults.tasks.lock().unwrap().drain(..) {
            if !handle.is_finished() {
                handle.abort();
                faults.aborted.fetch_add(1, Ordering::SeqCst);
            }
        }
        let outcome = call.await.unwrap();
        assert_eq!(outcome.result, "accepted");
        assert!(
            outcome.decided_at_ms < expires_at,
            "accepted decision must precede link expiry"
        );
        assert!(outcome.deadline_ms.unwrap() > outcome.decided_at_ms + 60_000);
        until("projector actually failed", || {
            faults.projection_failures.load(Ordering::SeqCst) > 0
        })
        .await;
        // SQL remains empty while admission, lifecycle startup, and revoke finish.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proof_links")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let before_revoke: LinkState = post(&client, &format!("{base}/status"), &())
            .await
            .json()
            .await
            .unwrap();
        accepted_states.push(Projection {
            link: link.clone(),
            state: before_revoke,
            events: vec!["request:winner".into(), "exhausted".into()],
        });
        timeout(
            Duration::from_secs(5),
            post(&client, &format!("{base}/revoke"), &()),
        )
        .await
        .expect("revoke waited for SQL");
        let fresh = Admission {
            operation: "fresh".into(),
            requester: "bob".into(),
            justification: "access".into(),
        };
        let rejected: Outcome = post(&client, &format!("{base}/admit"), &fresh)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(rejected.result, "revoked");
        let replay: Outcome = post(&client, &format!("{base}/admit"), &input)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(replay, outcome);
        let mut different = input.clone();
        different.justification = "changed".into();
        let conflict = client
            .post(format!("{base}/admit"))
            .json(&different)
            .send()
            .await
            .unwrap();
        assert_eq!(conflict.status(), reqwest::StatusCode::CONFLICT);
        let replay: Outcome = post(&client, &format!("{base}/admit"), &fresh)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(replay, rejected);
        println!(
            "PASS {stage}: task abort/replay, acceptance, SQL outage, revocation, replay/conflict"
        );
    }

    // Independent invocations compete for one object-local use.
    let race = format!("{}-race", ghinvite_core::RequestId::new());
    let base = format!("{ingress}/AdmissionProofLink/{race}");
    post(
        &client,
        &format!("{base}/initialize"),
        &(Utc::now().timestamp_millis() + 60_000),
    )
    .await;
    let a = Admission {
        operation: "a".into(),
        requester: "a".into(),
        justification: "".into(),
    };
    let b = Admission {
        operation: "b".into(),
        requester: "b".into(),
        justification: "".into(),
    };
    let url = format!("{base}/admit");
    let (a, b) = tokio::join!(post(&client, &url, &a), post(&client, &url, &b));
    let mut results = vec![
        a.json::<Outcome>().await.unwrap().result,
        b.json::<Outcome>().await.unwrap().result,
    ];
    results.sort();
    assert_eq!(results, ["accepted", "exhausted"]);

    // A new operation uses current time, while a durable decision is binding.
    let expired = format!("{}-expired", ghinvite_core::RequestId::new());
    let base = format!("{ingress}/AdmissionProofLink/{expired}");
    post(
        &client,
        &format!("{base}/initialize"),
        &(Utc::now().timestamp_millis() - 1),
    )
    .await;
    let input = Admission {
        operation: "expired".into(),
        requester: "a".into(),
        justification: "".into(),
    };
    let outcome: Outcome = post(&client, &format!("{base}/admit"), &input)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(outcome.result, "expired");

    // Contrast the binding decision with an interrupted attempt that has not
    // yet decided. Its restored state snapshot is stable, but its clock is new.
    let delayed = format!("{}-delayed", ghinvite_core::RequestId::new());
    let base = format!("{ingress}/AdmissionProofLink/{delayed}");
    let expires_at = Utc::now().timestamp_millis() + 2_000;
    post(&client, &format!("{base}/initialize"), &expires_at).await;
    faults.arm("before-decision");
    let call = tokio::spawn({
        let client = client.clone();
        let url = format!("{base}/admit");
        let input = input.clone();
        async move {
            post(&client, &url, &input)
                .await
                .json::<Outcome>()
                .await
                .unwrap()
        }
    });
    until("before-decision replay reached", || {
        faults.attempts.load(Ordering::SeqCst) >= 2
    })
    .await;
    while Utc::now().timestamp_millis() <= expires_at {
        sleep(Duration::from_millis(25)).await;
    }
    *faults.stage.lock().unwrap() = None;
    for handle in faults.tasks.lock().unwrap().drain(..) {
        if !handle.is_finished() {
            handle.abort();
            faults.aborted.fetch_add(1, Ordering::SeqCst);
        }
    }
    let outcome = call.await.unwrap();
    assert_eq!(outcome.result, "expired");
    assert!(outcome.decided_at_ms >= expires_at);
    println!(
        "PASS clock: interruption before decision expires; interruption after durable decision preserves acceptance"
    );

    until("five workflows started despite outage", || {
        faults.workflow_starts.load(Ordering::SeqCst) == 5
    })
    .await;
    faults.lose_ack.store(true, Ordering::SeqCst);
    faults.offline.store(false, Ordering::SeqCst);
    sqlx::query("DROP TRIGGER proof_offline")
        .execute(&pool)
        .await
        .unwrap();
    until("projection commit acknowledgement lost", || {
        faults.lost_acks.load(Ordering::SeqCst) == 1
    })
    .await;

    // Wait for the newest versions, then deliver old snapshots twice on purpose.
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let revoked: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proof_links WHERE revoked=1 AND revision=3")
                .fetch_one(&pool)
                .await
                .unwrap();
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proof_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        if revoked == 4 && events == 14 {
            break;
        }
        assert!(Instant::now() < deadline, "projection did not converge");
        sleep(Duration::from_millis(100)).await;
    }
    for projection in &accepted_states {
        for _ in 0..2 {
            post(
                &client,
                &format!("{ingress}/AdmissionProofProjection/apply"),
                projection,
            )
            .await;
        }
        let row = sqlx::query("SELECT revision, uses, revoked FROM proof_links WHERE id=?")
            .bind(&projection.link)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.get::<i64, _>("revision"), 3);
        assert_eq!(row.get::<i64, _>("uses"), 1);
        assert_eq!(row.get::<i64, _>("revoked"), 1);
    }
    let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proof_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proof_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    let uses: i64 = sqlx::query_scalar("SELECT SUM(uses) FROM proof_links")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((requests, uses, events), (5, 5, 14));
    assert!(faults.aborted.load(Ordering::SeqCst) >= 4);
    assert_eq!(faults.workflow_starts.load(Ordering::SeqCst), 5);
    println!(
        "PASS convergence: 5 requests, 5 uses, 14 events; late duplicate snapshots cannot undo revocation; real SQL commit acknowledgement loss retried"
    );
    split_state::scenario(&client, &ingress, &faults, &pool).await;
    server.abort();
}
