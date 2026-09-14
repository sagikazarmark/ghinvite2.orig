//! Extension of the throwaway protocol proof: lazy, bounded-access state.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Header {
    revision: u64,
    uses: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct RequestRecord {
    outcome: Outcome,
    state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Decision {
    header: Header,
    outcome: Outcome,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct StatusQuery {
    operation: String,
    requester: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Status {
    header: Header,
    outcome: Option<Outcome>,
    request: Option<RequestRecord>,
    blocker: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct Transition {
    operation: String,
    state: String,
}

#[restate_sdk::object]
trait AdmissionSplitProof {
    async fn initialize() -> Result<(), TerminalError>;
    async fn admit(input: Json<Admission>) -> Result<Json<Outcome>, TerminalError>;
    // Deliberately exclusive: a multi-record authoritative view must queue
    // behind unfinished mutations, including their suspension and replay.
    async fn status(query: Json<StatusQuery>) -> Result<Json<Status>, TerminalError>;
    async fn transition(input: Json<Transition>) -> Result<(), TerminalError>;
}

struct Handler(Arc<Faults>);

impl AdmissionSplitProof for Handler {
    async fn initialize(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        ctx.set(
            "v1/link",
            Json(Header {
                revision: 0,
                uses: 0,
            }),
        );
        // A recognizable, unrelated 256 KiB history entry. The HTTP observer
        // asserts it is not eagerly sent to status/admission executions.
        ctx.set("v1/op/history", "UNRELATED_HISTORY_PAYLOAD".repeat(11_000));
        Ok(())
    }

    async fn admit(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Admission>,
    ) -> Result<Json<Outcome>, TerminalError> {
        let operation_key = format!("v1/op/{}", input.operation);
        if let Some(Json(outcome)) = ctx.get::<Json<Outcome>>(&operation_key).await? {
            if outcome.input != input {
                return Err(TerminalError::new_with_code(
                    409,
                    "operation payload conflict",
                ));
            }
            return Ok(Json(outcome));
        }
        let Json(header) = ctx.get::<Json<Header>>("v1/link").await?.unwrap();
        let blocker_key = format!("v1/blocker/{}", input.requester);
        let blocker = ctx.get::<String>(&blocker_key).await?;
        let Json(decision) = ctx
            .run(|| async {
                let now = Utc::now().timestamp_millis();
                let mut header = header.clone();
                let accepted = blocker.is_none();
                header.revision += 1;
                if accepted {
                    header.uses += 1;
                }
                Ok::<_, HandlerError>(Json(Decision {
                    header,
                    outcome: Outcome {
                        input: input.clone(),
                        result: if accepted {
                            "accepted"
                        } else {
                            "existing-request"
                        }
                        .into(),
                        decided_at_ms: now,
                        deadline_ms: accepted.then_some(now + 604_800_000),
                    },
                }))
            })
            .name("decide_split")
            .await?;

        ctx.set("v1/link", Json(decision.header.clone()));
        ctx.run(|| self.0.checkpoint("split-after-header"))
            .name("split-after-header")
            .retry_policy(retry_policy())
            .await?;
        if decision.outcome.result == "accepted" {
            ctx.set(
                &format!("v1/request/{}", input.operation),
                Json(RequestRecord {
                    outcome: decision.outcome.clone(),
                    state: "pending".into(),
                }),
            );
            ctx.run(|| self.0.checkpoint("split-after-request"))
                .name("split-after-request")
                .retry_policy(retry_policy())
                .await?;
            ctx.set(&blocker_key, input.operation.clone());
            ctx.run(|| self.0.checkpoint("split-after-blocker"))
                .name("split-after-blocker")
                .retry_policy(retry_policy())
                .await?;
        }
        ctx.set(&operation_key, Json(decision.outcome.clone()));
        ctx.run(|| self.0.checkpoint("split-after-outcome"))
            .name("split-after-outcome")
            .retry_policy(retry_policy())
            .await?;

        // Bounded projection message: current link snapshot + only this request,
        // never the accumulated operation/request history.
        let outcomes = BTreeMap::from([(input.operation.clone(), decision.outcome.clone())]);
        ctx.service_client::<AdmissionProofProjectionClient>()
            .apply(Json(Projection {
                link: ctx.key().into(),
                state: LinkState {
                    revision: decision.header.revision,
                    uses: decision.header.uses,
                    outcomes,
                    ..Default::default()
                },
                events: if decision.outcome.result == "accepted" {
                    vec![format!("request:{}", input.operation)]
                } else {
                    vec![]
                },
            }))
            .send()
            .await?;
        if decision.outcome.result == "accepted" {
            ctx.workflow_client::<AdmissionProofRequestClient>(format!(
                "{}-{}",
                ctx.key(),
                input.operation
            ))
            .run(Json(decision.outcome.clone()))
            .send()
            .await?;
        }
        Ok(Json(decision.outcome))
    }

    async fn status(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<StatusQuery>,
    ) -> Result<Json<Status>, TerminalError> {
        let Json(header) = ctx.get::<Json<Header>>("v1/link").await?.unwrap();
        let outcome = ctx
            .get::<Json<Outcome>>(&format!("v1/op/{}", query.operation))
            .await?
            .map(|Json(v)| v);
        let request = ctx
            .get::<Json<RequestRecord>>(&format!("v1/request/{}", query.operation))
            .await?
            .map(|Json(v)| v);
        let blocker = ctx
            .get::<String>(&format!("v1/blocker/{}", query.requester))
            .await?;
        Ok(Json(Status {
            header,
            outcome,
            request,
            blocker,
        }))
    }

    async fn transition(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Transition>,
    ) -> Result<(), TerminalError> {
        let request_key = format!("v1/request/{}", input.operation);
        let Json(mut request) = ctx.get::<Json<RequestRecord>>(&request_key).await?.unwrap();
        if request.state != "pending" {
            return Ok(());
        }
        assert!(["approved", "declined", "expired", "cancelled"].contains(&input.state.as_str()));
        let blocker_key = format!("v1/blocker/{}", request.outcome.input.requester);
        let blocker = ctx.get::<String>(&blocker_key).await?;
        request.state = input.state.clone();
        ctx.set(&request_key, Json(request));
        ctx.run(|| self.0.checkpoint("split-after-terminal"))
            .name("split-after-terminal")
            .retry_policy(retry_policy())
            .await?;
        if input.state != "approved" && blocker.as_deref() == Some(&input.operation) {
            ctx.clear(&blocker_key);
        }
        // This deliberately proves blocker coordination only; production also
        // needs revisioned lifecycle projection, audit, and deadline arbitration.
        Ok(())
    }
}

pub(super) fn bind(
    builder: restate_sdk::endpoint::Builder,
    faults: Arc<Faults>,
) -> restate_sdk::endpoint::Builder {
    builder.bind_with_options(
        Handler(faults).serve(),
        restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true),
    )
}

async fn release(faults: &Faults) {
    *faults.stage.lock().unwrap() = None;
    for handle in faults.tasks.lock().unwrap().drain(..) {
        if !handle.is_finished() {
            handle.abort();
            faults.aborted.fetch_add(1, Ordering::SeqCst);
        }
    }
}

pub(super) async fn scenario(
    client: &reqwest::Client,
    ingress: &str,
    faults: &Arc<Faults>,
    pool: &SqlitePool,
) {
    for stage in [
        "split-after-header",
        "split-after-request",
        "split-after-blocker",
        "split-after-outcome",
    ] {
        let link = format!("{}-{stage}", ghinvite_core::RequestId::new());
        let base = format!("{ingress}/AdmissionSplitProof/{link}");
        post(client, &format!("{base}/initialize"), &()).await;
        faults.arm(stage);
        let input = Admission {
            operation: "original".into(),
            requester: "alice".into(),
            justification: "bounded".into(),
        };
        let submit = tokio::spawn({
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
        until("split-key replay reached", || {
            faults.attempts.load(Ordering::SeqCst) >= 2
        })
        .await;
        let mut status = tokio::spawn({
            let client = client.clone();
            let url = format!("{base}/status");
            async move {
                post(
                    &client,
                    &url,
                    &StatusQuery {
                        operation: "original".into(),
                        requester: "alice".into(),
                    },
                )
                .await
                .json::<Status>()
                .await
                .unwrap()
            }
        });
        assert!(
            timeout(Duration::from_millis(200), &mut status)
                .await
                .is_err(),
            "exclusive status exposed a partial transition"
        );
        release(faults).await;
        let accepted = submit.await.unwrap();
        let view = status.await.unwrap();
        assert_eq!(view.header.uses, 1);
        assert_eq!(view.outcome.as_ref(), Some(&accepted));
        assert_eq!(view.request.as_ref().unwrap().state, "pending");
        assert_eq!(view.blocker.as_deref(), Some("original"));
        let replay: Outcome = post(client, &format!("{base}/admit"), &input)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(replay, accepted);
        let changed = Admission {
            justification: "different".into(),
            ..input.clone()
        };
        assert_eq!(
            client
                .post(format!("{base}/admit"))
                .json(&changed)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::CONFLICT
        );
        let next = Admission {
            operation: "next".into(),
            ..input.clone()
        };
        let blocked: Outcome = post(client, &format!("{base}/admit"), &next)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(blocked.result, "existing-request");

        if stage == "split-after-outcome" {
            faults.arm("split-after-terminal");
            let transition = tokio::spawn({
                let client = client.clone();
                let url = format!("{base}/transition");
                async move {
                    post(
                        &client,
                        &url,
                        &Transition {
                            operation: "original".into(),
                            state: "declined".into(),
                        },
                    )
                    .await;
                }
            });
            until("terminal transition replay reached", || {
                faults.attempts.load(Ordering::SeqCst) >= 2
            })
            .await;
            release(faults).await;
            transition.await.unwrap();
        } else {
            let terminal = if stage == "split-after-header" {
                "expired"
            } else {
                "cancelled"
            };
            post(
                client,
                &format!("{base}/transition"),
                &Transition {
                    operation: "original".into(),
                    state: terminal.into(),
                },
            )
            .await;
        }
        // The rejected operation remains rejected after eligibility changes.
        let replay: Outcome = post(client, &format!("{base}/admit"), &next)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(replay, blocked);
        let new = Admission {
            operation: "new".into(),
            ..input.clone()
        };
        let admitted: Outcome = post(client, &format!("{base}/admit"), &new)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(admitted.result, "accepted");
        // A stale release for the old request must not clear the new blocker.
        post(
            client,
            &format!("{base}/transition"),
            &Transition {
                operation: "original".into(),
                state: "declined".into(),
            },
        )
        .await;
        post(
            client,
            &format!("{base}/transition"),
            &Transition {
                operation: "new".into(),
                state: "approved".into(),
            },
        )
        .await;
        let third = Admission {
            operation: "third".into(),
            ..input
        };
        let blocked: Outcome = post(client, &format!("{base}/admit"), &third)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(blocked.result, "existing-request");
        let view: Status = post(
            client,
            &format!("{base}/status"),
            &StatusQuery {
                operation: "new".into(),
                requester: "alice".into(),
            },
        )
        .await
        .json()
        .await
        .unwrap();
        assert_eq!(view.header.uses, 2);
        assert_eq!(view.blocker.as_deref(), Some("new"));
        assert_eq!(view.request.unwrap().state, "approved");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proof_requests WHERE link=?")
                .bind(&link)
                .fetch_one(pool)
                .await
                .unwrap();
            if count == 2 {
                break;
            }
            assert!(Instant::now() < deadline, "split projections missing");
            sleep(Duration::from_millis(25)).await;
        }
        println!(
            "PASS {stage}: exclusive coherent reads, replay, two nonrefunded uses, terminal release, approved suppression, stale release"
        );
    }
    assert!(
        !faults.history_leaked.load(Ordering::SeqCst),
        "unrelated history was eagerly transferred"
    );
    assert!(
        faults.split_request_bytes.load(Ordering::SeqCst) < 64 * 1024,
        "split request payload grew with unrelated history"
    );
    println!(
        "PASS lazy loading: unrelated 256 KiB history absent; largest split invocation body {} bytes",
        faults.split_request_bytes.load(Ordering::SeqCst)
    );
}
