//! Direct workflow promise behavior on the pinned runtime, including retention.
use super::*;
use restate_sdk::context::{ContextPromises, SharedWorkflowContext};
use restate_sdk::endpoint::{HandlerOptions, ServiceOptions};
use restate_sdk::service::IntoServiceDefinition;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
struct Fact {
    request: String,
    decision: String,
    state: String,
}

struct NotificationProof(Arc<Faults>);

#[restate_sdk::workflow]
impl NotificationProof {
    #[handler]
    async fn run(&self, ctx: WorkflowContext<'_>) -> Result<Json<Fact>, TerminalError> {
        ctx.run(|| async {
            self.0.notification_runs.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HandlerError>(())
        })
        .name("observe-run")
        .await?;
        ctx.promise::<Json<Fact>>("terminal").await
    }

    #[handler]
    async fn notify(
        &self,
        ctx: SharedWorkflowContext<'_>,
        Json(fact): Json<Fact>,
    ) -> Result<String, TerminalError> {
        if let Some(Json(previous)) = ctx.peek_promise::<Json<Fact>>("terminal").await? {
            if previous != fact {
                return Err(TerminalError::new_with_code(
                    409,
                    "conflicting terminal fact",
                ));
            }
            return Ok("already-delivered".into());
        }
        ctx.resolve_promise("terminal", Json(fact));
        ctx.run(|| self.0.checkpoint("notification-after-resolve"))
            .name("notification-after-resolve")
            .retry_policy(retry_policy())
            .await?;
        Ok("delivered".into())
    }

    #[handler]
    async fn peek(
        &self,
        ctx: SharedWorkflowContext<'_>,
    ) -> Result<Json<Option<Fact>>, TerminalError> {
        Ok(Json(
            ctx.peek_promise::<Json<Fact>>("terminal")
                .await?
                .map(|Json(fact)| fact),
        ))
    }
}

pub(super) fn bind(
    builder: restate_sdk::endpoint::Builder,
    faults: Arc<Faults>,
) -> restate_sdk::endpoint::Builder {
    builder.bind(NotificationProof(faults).into_service_definition().options(
        ServiceOptions::new().handler(
            "run",
            HandlerOptions::new().workflow_retention(Duration::from_secs(2)),
        ),
    ))
}

pub(super) async fn scenario(client: &reqwest::Client, ingress: &str, faults: &Arc<Faults>) {
    let key = ghinvite_core::RequestId::new().to_string();
    let base = format!("{ingress}/NotificationProof/{key}");
    let fact = Fact {
        request: key.clone(),
        decision: "terminal-1".into(),
        state: "approved".into(),
    };
    let early = client
        .post(format!("{base}/notify"))
        .json(&fact)
        .send()
        .await
        .unwrap();
    let early_status = early.status();
    assert!(early_status.is_success());
    println!("NOTIFICATION before-start HTTP {early_status}");
    // A shared handler must not secretly start the workflow run handler.
    let peek = client.post(format!("{base}/peek")).send().await.unwrap();
    println!("NOTIFICATION peek-before-start HTTP {}", peek.status());
    assert_eq!(
        peek.json::<Option<Fact>>().await.unwrap(),
        Some(fact.clone())
    );
    assert_eq!(faults.notification_runs.load(Ordering::SeqCst), 0);

    let run = tokio::spawn({
        let client = client.clone();
        let url = format!("{base}/run");
        async move { post(&client, &url, &()).await.json::<Fact>().await.unwrap() }
    });
    // Explicitly observe the shared handler becoming accessible before sending
    // a notification that must succeed. Do not replace a missing workflow with
    // a mock success, and do not assume a fixed startup delay is sufficient.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let peek = client.post(format!("{base}/peek")).send().await.unwrap();
        if peek.status().is_success() {
            break;
        }
        assert!(Instant::now() < deadline, "workflow never became available");
        sleep(Duration::from_millis(20)).await;
    }
    // No second notification: prove the fact sent before startup is consumed.
    assert_eq!(run.await.unwrap(), fact);
    let duplicate: String = post(client, &format!("{base}/notify"), &fact)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(duplicate, "already-delivered");
    let changed = Fact {
        decision: "different".into(),
        state: "declined".into(),
        ..fact.clone()
    };
    let conflict = client
        .post(format!("{base}/notify"))
        .json(&changed)
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), reqwest::StatusCode::CONFLICT);

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let peek = client.post(format!("{base}/peek")).send().await.unwrap();
        let status = peek.status();
        let value = if status.is_success() {
            peek.json::<Option<Fact>>().await.unwrap()
        } else {
            Some(fact.clone())
        };
        if value.is_none() {
            println!("NOTIFICATION after-retention peek HTTP {status}, promise absent");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "two-second workflow retention did not clean up"
        );
        sleep(Duration::from_millis(100)).await;
    }
    let late = client
        .post(format!("{base}/notify"))
        .json(&fact)
        .send()
        .await
        .unwrap();
    println!("NOTIFICATION after-retention notify HTTP {}", late.status());
    assert!(late.status().is_success());
    let republished: Option<Fact> = post(client, &format!("{base}/peek"), &())
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(republished, Some(fact.clone()));
    assert_eq!(
        faults.notification_runs.load(Ordering::SeqCst),
        1,
        "late notification restarted workflow"
    );

    // Independent workflow for the interruption experiment.
    let key = ghinvite_core::RequestId::new().to_string();
    let base = format!("{ingress}/NotificationProof/{key}");
    let fact = Fact {
        request: key,
        decision: "terminal-2".into(),
        state: "expired".into(),
    };
    let run = tokio::spawn({
        let client = client.clone();
        let url = format!("{base}/run");
        async move { post(&client, &url, &()).await.json::<Fact>().await.unwrap() }
    });
    until("second workflow started and waiting", || {
        faults.notification_runs.load(Ordering::SeqCst) == 2
    })
    .await;
    assert!(!run.is_finished());
    faults.arm("notification-after-resolve");
    let notify = tokio::spawn({
        let client = client.clone();
        let url = format!("{base}/notify");
        let fact = fact.clone();
        async move { post(&client, &url, &fact).await }
    });
    until("notification resolution replay", || {
        faults.attempts.load(Ordering::SeqCst) >= 2
    })
    .await;
    *faults.stage.lock().unwrap() = None;
    for task in faults.tasks.lock().unwrap().drain(..) {
        if !task.is_finished() {
            task.abort();
        }
    }
    notify.await.unwrap();
    assert_eq!(run.await.unwrap(), fact);
    println!(
        "PASS direct notifications: waiting delivery, identical replay, conflicting fact, interrupted resolution; retention gap observed"
    );
}
