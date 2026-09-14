//! Throwaway deadline proof. Fixed requester, compact link record, split requests.
//! Controlled time is test infrastructure, never command-supplied production time.
use super::*;
use restate_sdk::context::ContextTimers;
use std::sync::atomic::AtomicI64;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Link {
    uses: u32,
    max_uses: u32,
    revision: u64,
    blocker: Option<String>,
    auto: bool,
    lifetime: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
struct Record {
    id: String,
    state: String,
    deadline: Option<i64>,
    effective_at: i64,
    evaluated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Command {
    id: String,
    action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Change {
    link: Link,
    expired: Option<Record>,
    request: Option<Record>,
    result: String,
    events: Vec<String>,
}

#[derive(Clone)]
struct Handler {
    faults: Arc<Faults>,
    clock: Arc<AtomicI64>,
}

#[restate_sdk::object]
trait DeadlineProof {
    async fn initialize(link: Json<Link>) -> Result<(), TerminalError>;
    async fn command(command: Json<Command>) -> Result<Json<Change>, TerminalError>;
    async fn inspect(id: String) -> Result<Json<(Link, Option<Record>)>, TerminalError>;
}

impl DeadlineProof for Handler {
    async fn initialize(
        &self,
        ctx: ObjectContext<'_>,
        link: Json<Link>,
    ) -> Result<(), TerminalError> {
        ctx.set("link", link);
        Ok(())
    }

    async fn inspect(
        &self,
        ctx: ObjectContext<'_>,
        id: String,
    ) -> Result<Json<(Link, Option<Record>)>, TerminalError> {
        let Json(link) = ctx.get::<Json<Link>>("link").await?.unwrap();
        let request = ctx
            .get::<Json<Record>>(&format!("request/{id}"))
            .await?
            .map(|Json(r)| r);
        Ok(Json((link, request)))
    }

    async fn command(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<Command>,
    ) -> Result<Json<Change>, TerminalError> {
        let admission = command.action == "admit";
        if admission
            && let Some(result) = ctx
                .get::<Json<Change>>(&format!("op/{}", command.id))
                .await?
        {
            return Ok(result);
        }
        let Json(link) = ctx.get::<Json<Link>>("link").await?.unwrap();
        let target = if admission {
            link.blocker.as_deref()
        } else {
            Some(command.id.as_str())
        };
        let request = if let Some(id) = target {
            ctx.get::<Json<Record>>(&format!("request/{id}"))
                .await?
                .map(|Json(r)| r)
        } else {
            None
        };
        ctx.run(|| self.faults.checkpoint("deadline-before-decision"))
            .name("deadline-before-decision")
            .retry_policy(retry_policy())
            .await?;
        let Json(change) = ctx
            .run(|| async {
                let configured = self.clock.load(Ordering::SeqCst);
                let now = if configured < 0 {
                    Utc::now().timestamp_millis()
                } else {
                    configured
                };
                let mut link = link.clone();
                let mut request = request.clone();
                let mut expired = None;
                let mut events = Vec::new();
                if let Some(r) = &mut request
                    && r.state == "pending"
                    && now >= r.deadline.unwrap()
                {
                    r.state = "expired".into();
                    r.effective_at = r.deadline.unwrap();
                    r.evaluated_at = now;
                    if link.blocker.as_deref() == Some(&r.id) {
                        link.blocker = None;
                    }
                    expired = Some(r.clone());
                    events.push(format!("expired:{}", r.id));
                }
                let result;
                if admission {
                    result = if link.blocker.is_some() {
                        "blocked"
                    } else if link.uses >= link.max_uses {
                        "exhausted"
                    } else {
                        "accepted"
                    }
                    .to_string();
                    request = None;
                    if result == "accepted" {
                        link.uses += 1;
                        link.blocker = Some(command.id.clone());
                        request = Some(Record {
                            id: command.id.clone(),
                            state: if link.auto { "approved" } else { "pending" }.into(),
                            deadline: (!link.auto).then_some(now + link.lifetime),
                            effective_at: now,
                            evaluated_at: now,
                        });
                        events.push(format!("created:{}", command.id));
                    }
                } else {
                    let r = request.as_mut().expect("known proof request");
                    if r.state == "pending"
                        && ["approve", "decline"].contains(&command.action.as_str())
                    {
                        r.state = if command.action == "approve" {
                            "approved"
                        } else {
                            "declined"
                        }
                        .into();
                        r.effective_at = now;
                        r.evaluated_at = now;
                        if r.state == "declined" && link.blocker.as_deref() == Some(&r.id) {
                            link.blocker = None;
                        }
                        events.push(format!("{}:{}", r.state, r.id));
                    }
                    result = r.state.clone();
                }
                link.revision += 1;
                Ok::<_, HandlerError>(Json(Change {
                    link,
                    expired,
                    request,
                    result,
                    events,
                }))
            })
            .name("deadline-decision")
            .await?;
        ctx.run(|| self.faults.checkpoint("deadline-after-decision"))
            .name("deadline-after-decision")
            .retry_policy(retry_policy())
            .await?;
        if let Some(expired) = &change.expired {
            ctx.set(&format!("request/{}", expired.id), Json(expired.clone()));
        }
        ctx.run(|| self.faults.checkpoint("deadline-after-expiry"))
            .name("deadline-after-expiry")
            .retry_policy(retry_policy())
            .await?;
        ctx.set("link", Json(change.link.clone()));
        ctx.run(|| self.faults.checkpoint("deadline-after-link"))
            .name("deadline-after-link")
            .retry_policy(retry_policy())
            .await?;
        if let Some(request) = &change.request {
            ctx.set(&format!("request/{}", request.id), Json(request.clone()));
        }
        ctx.run(|| self.faults.checkpoint("deadline-after-request"))
            .name("deadline-after-request")
            .retry_policy(retry_policy())
            .await?;
        if admission {
            ctx.set(&format!("op/{}", command.id), Json(change.clone()));
        }
        ctx.service_client::<AdmissionProofProjectionClient>()
            .apply(Json(Projection {
                link: ctx.key().into(),
                state: LinkState {
                    uses: change.link.uses,
                    revision: change.link.revision,
                    ..Default::default()
                },
                events: change.events.clone(),
            }))
            .send()
            .await?;
        ctx.run(|| self.faults.checkpoint("deadline-after-send"))
            .name("deadline-after-send")
            .retry_policy(retry_policy())
            .await?;
        Ok(Json(change))
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct Wake {
    link: String,
    request: String,
    deadline: i64,
}

#[restate_sdk::workflow]
trait DeadlineTimerProof {
    async fn run(wake: Json<Wake>) -> Result<String, TerminalError>;
}
struct Timer;
impl DeadlineTimerProof for Timer {
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(wake): Json<Wake>,
    ) -> Result<String, TerminalError> {
        loop {
            let now = ctx
                .run(|| async { Ok::<_, HandlerError>(Utc::now().timestamp_millis()) })
                .name("wake-clock")
                .await?;
            if wake.deadline > now {
                ctx.sleep(Duration::from_millis((wake.deadline - now) as u64))
                    .await?;
            }
            let Json(result) = ctx
                .object_client::<DeadlineProofClient>(&wake.link)
                .command(Json(Command {
                    id: wake.request.clone(),
                    action: "expire".into(),
                }))
                .call()
                .await?;
            if result.result != "pending" {
                return Ok(result.result);
            }
        }
    }
}

pub(super) fn bind(
    builder: restate_sdk::endpoint::Builder,
    faults: Arc<Faults>,
    clock: Arc<AtomicI64>,
) -> restate_sdk::endpoint::Builder {
    builder
        .bind_with_options(
            Handler { faults, clock }.serve(),
            restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true),
        )
        .bind(Timer.serve())
}

async fn command(client: &reqwest::Client, base: &str, id: &str, action: &str) -> Change {
    post(
        client,
        &format!("{base}/command"),
        &Command {
            id: id.into(),
            action: action.into(),
        },
    )
    .await
    .json()
    .await
    .unwrap()
}

async fn setup(
    client: &reqwest::Client,
    ingress: &str,
    max_uses: u32,
    auto: bool,
    lifetime: i64,
) -> (String, String) {
    let key = ghinvite_core::RequestId::new().to_string();
    let base = format!("{ingress}/DeadlineProof/{key}");
    post(
        client,
        &format!("{base}/initialize"),
        &Link {
            uses: 0,
            max_uses,
            revision: 0,
            blocker: None,
            auto,
            lifetime,
        },
    )
    .await;
    (key, base)
}

async fn abort(faults: &Faults) {
    *faults.stage.lock().unwrap() = None;
    for task in faults.tasks.lock().unwrap().drain(..) {
        if !task.is_finished() {
            task.abort();
        }
    }
}

pub(super) async fn scenario(
    client: &reqwest::Client,
    ingress: &str,
    faults: &Arc<Faults>,
    clock: &Arc<AtomicI64>,
    pool: &SqlitePool,
) {
    for action in ["approve", "decline"] {
        for now in [99, 100, 101] {
            clock.store(0, Ordering::SeqCst);
            let (_, base) = setup(client, ingress, 2, false, 100).await;
            command(client, &base, "old", "admit").await;
            clock.store(now, Ordering::SeqCst);
            let result = command(client, &base, "old", action).await;
            assert_eq!(
                result.result,
                if now < 100 {
                    if action == "approve" {
                        "approved"
                    } else {
                        "declined"
                    }
                } else {
                    "expired"
                }
            );
            if now >= 100 {
                assert_eq!(result.request.unwrap().effective_at, 100);
            }
        }
    }
    for stage in ["deadline-before-decision", "deadline-after-decision"] {
        clock.store(0, Ordering::SeqCst);
        let (_, base) = setup(client, ingress, 2, false, 100).await;
        command(client, &base, "old", "admit").await;
        clock.store(99, Ordering::SeqCst);
        faults.arm(stage);
        let call = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move { command(&client, &base, "old", "approve").await }
        });
        until("deadline decision replay", || {
            faults.attempts.load(Ordering::SeqCst) >= 2
        })
        .await;
        clock.store(101, Ordering::SeqCst);
        abort(faults).await;
        assert_eq!(
            call.await.unwrap().result,
            if stage == "deadline-before-decision" {
                "expired"
            } else {
                "approved"
            }
        );
    }
    for (stage, max) in [
        ("deadline-after-expiry", 2),
        ("deadline-after-link", 2),
        ("deadline-after-request", 2),
        ("deadline-after-send", 2),
        ("deadline-after-expiry", 1),
    ] {
        clock.store(0, Ordering::SeqCst);
        let (key, base) = setup(client, ingress, max, false, 100).await;
        let original = command(client, &base, "old", "admit").await;
        clock.store(100, Ordering::SeqCst);
        faults.arm(stage);
        let call = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move { command(&client, &base, "new", "admit").await }
        });
        until("expiry/readmission replay", || {
            faults.attempts.load(Ordering::SeqCst) >= 2
        })
        .await;
        abort(faults).await;
        let result = call.await.unwrap();
        assert_eq!(
            result.result,
            if max == 2 { "accepted" } else { "exhausted" }
        );
        assert_eq!(result.link.uses, max);
        let replay = command(client, &base, "old", "admit").await;
        assert_eq!(replay.request, original.request);
        command(client, &base, "old", "expire").await;
        let record: (Link, Option<Record>) = post(client, &format!("{base}/inspect"), &"old")
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(record.1.unwrap().state, "expired");
        assert_eq!(
            record.0.blocker.as_deref(),
            if max == 2 { Some("new") } else { None }
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM proof_events WHERE link=? AND event='expired:old'",
            )
            .bind(&key)
            .fetch_one(pool)
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            assert!(Instant::now() < deadline, "expiry event missing");
            sleep(Duration::from_millis(25)).await;
        }
    }
    // Authoritative status materializes expiry; early expiry request does not.
    clock.store(0, Ordering::SeqCst);
    let (_, base) = setup(client, ingress, 2, false, 100).await;
    command(client, &base, "old", "admit").await;
    assert_eq!(
        command(client, &base, "old", "expire").await.result,
        "pending"
    );
    clock.store(100, Ordering::SeqCst);
    assert_eq!(
        command(client, &base, "old", "status").await.result,
        "expired"
    );
    let (_, base) = setup(client, ingress, 2, true, 100).await;
    let auto = command(client, &base, "auto", "admit")
        .await
        .request
        .unwrap();
    assert_eq!(auto.state, "approved");
    assert_eq!(auto.deadline, None);
    // Real timer race and already-overdue workflow startup, no controlled clock.
    clock.store(-1, Ordering::SeqCst);
    for delayed in [false, true] {
        let (key, base) = setup(client, ingress, 2, false, 200).await;
        let admitted = command(client, &base, "old", "admit").await;
        let deadline = admitted.request.unwrap().deadline.unwrap();
        if delayed {
            sleep(Duration::from_millis(250)).await;
        }
        let timer = tokio::spawn({
            let client = client.clone();
            let url = format!("{ingress}/DeadlineTimerProof/{key}/run");
            let key = key.clone();
            async move {
                post(
                    &client,
                    &url,
                    &Wake {
                        link: key,
                        request: "old".into(),
                        deadline,
                    },
                )
                .await
                .json::<String>()
                .await
                .unwrap()
            }
        });
        while Utc::now().timestamp_millis() <= deadline {
            sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            command(client, &base, "old", "approve").await.result,
            "expired"
        );
        assert_eq!(timer.await.unwrap(), "expired");
    }
    println!(
        "PASS deadlines: strict before/equal/after; timely replay; expiry+readmission/rejection interruptions; status expiry; auto-approval; real timer races/delayed startup"
    );
}
