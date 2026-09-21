//! Direct terminal promises coordinate wake-up; only the link authorizes work.
use crate::admission::{
    InvitationLinkClient, InvitationRequest, RequestStatus, TerminalSignal, WorkflowEnvelope,
};
use ghinvite_core::RequestState;
use restate_sdk::context::{
    ContextClient, ContextPromises, ContextSideEffects, ContextTimers, RunFuture,
    SharedWorkflowContext, WorkflowContext,
};
use restate_sdk::endpoint::Builder;
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowResult {
    pub state: RequestState,
    /// Stable handoff for #56. Approval does not claim external delivery.
    pub dispatch: Option<ApprovedDispatch>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovedDispatch {
    pub dispatch_id: String,
    pub input: WorkflowEnvelope,
    pub commands: Vec<ghinvite_core::delivery::CreateCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubmittedCommand {
    pub command: ghinvite_core::delivery::CreateCommand,
    pub invocation_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryStatus {
    pub plan: ApprovedDispatch,
    pub submitted: Vec<SubmittedCommand>,
}

#[derive(Default)]
pub struct InvitationRequestImpl {
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<WorkflowFaults>>,
}

#[cfg(feature = "integration")]
#[derive(Default)]
pub struct WorkflowFaults {
    pub pause_before_dispatch: std::sync::atomic::AtomicBool,
    pub pause_after_send: std::sync::atomic::AtomicBool,
    pub sent_before_pause: std::sync::atomic::AtomicUsize,
    pub early_wakes: std::sync::atomic::AtomicUsize,
    pub interrupt_notification: std::sync::atomic::AtomicBool,
    pub notification_attempts: std::sync::atomic::AtomicUsize,
    pub interrupt_timer: std::sync::atomic::AtomicBool,
    pub timer_attempts: std::sync::atomic::AtomicUsize,
    pub waits: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "integration")]
pub fn bind_with_faults(builder: Builder, faults: std::sync::Arc<WorkflowFaults>) -> Builder {
    use restate_sdk::endpoint::{HandlerOptions, ServiceOptions};
    builder.bind_with_options(
        InvitationRequestImpl {
            faults: Some(faults),
        }
        .serve(),
        ServiceOptions::new()
            .journal_retention(std::time::Duration::from_secs(2))
            .idempotency_retention(std::time::Duration::from_secs(2))
            .handler(
                "run",
                HandlerOptions::new().workflow_retention(std::time::Duration::from_secs(2)),
            ),
    )
}

pub fn bind(builder: Builder) -> Builder {
    builder.bind(InvitationRequestImpl::default().serve())
}

impl InvitationRequest for InvitationRequestImpl {
    async fn notification_status(
        &self,
        ctx: SharedWorkflowContext<'_>,
    ) -> Result<Json<Option<TerminalSignal>>, TerminalError> {
        Ok(Json(
            ctx.peek_promise::<Json<TerminalSignal>>("terminal")
                .await?
                .map(|s| s.0),
        ))
    }
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(input): Json<WorkflowEnvelope>,
    ) -> Result<Json<WorkflowResult>, TerminalError> {
        if ctx.key() != input.request.request_id.to_string() {
            return Err(TerminalError::new_with_code(400, "invalid workflow input"));
        }
        let query = RequestStatus {
            link_id: input.request.link_id,
            request_id: input.request.request_id,
            requester_id: input.request.requester_id,
        };
        let mut signalled = false;
        loop {
            let Json(request) = ctx
                .object_client::<InvitationLinkClient>(query.link_id.to_string())
                .request_status(Json(query.clone()))
                .call()
                .await?;
            if request.state != RequestState::Pending {
                let state = request.state;
                #[cfg(feature = "integration")]
                if let Some(faults) = &self.faults {
                    ctx.run(|| async {
                        if faults
                            .pause_before_dispatch
                            .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            return Err(std::io::Error::other(
                                "fixture: pause before dispatch preparation",
                            )
                            .into());
                        }
                        Ok::<_, HandlerError>(())
                    })
                    .name("before_dispatch")
                    .retry_policy(
                        restate_sdk::context::RunRetryPolicy::default()
                            .initial_delay(std::time::Duration::from_millis(100)),
                    )
                    .await?;
                }
                let dispatch = if state == RequestState::Approved {
                    Some(
                        ctx.object_client::<InvitationLinkClient>(query.link_id.to_string())
                            .prepare_dispatch(Json(query.clone()))
                            .call()
                            .await?
                            .0,
                    )
                } else {
                    None
                };
                if let Some(plan) = &dispatch {
                    for command in &plan.commands {
                        let invocation_id = ctx
                            .object_client::<crate::delivery::GithubCreateClient>(
                                command.invitation_id.to_string(),
                            )
                            .create(Json(command.clone()))
                            .send()
                            .await?
                            .invocation_id()
                            .to_owned();
                        #[cfg(feature = "integration")]
                        if let Some(faults) = &self.faults {
                            ctx.run(|| async {
                                use std::sync::atomic::Ordering;
                                if faults.pause_after_send.load(Ordering::SeqCst) {
                                    faults.sent_before_pause.fetch_add(1, Ordering::SeqCst);
                                    return Err(std::io::Error::other(
                                        "fixture: send acknowledged before checkpoint loss",
                                    )
                                    .into());
                                }
                                Ok::<_, HandlerError>(())
                            })
                            .name("after_repository_send")
                            .retry_policy(
                                restate_sdk::context::RunRetryPolicy::default()
                                    .initial_delay(std::time::Duration::from_millis(100)),
                            )
                            .await?;
                        }
                        ctx.object_client::<InvitationLinkClient>(query.link_id.to_string())
                            .record_submitted(Json(SubmittedCommand {
                                command: command.clone(),
                                invocation_id,
                            }))
                            .call()
                            .await?;
                    }
                }
                let decision = request
                    .decision
                    .as_ref()
                    .ok_or_else(|| TerminalError::new("terminal decision missing"))?;
                ctx.object_client::<InvitationLinkClient>(query.link_id.to_string())
                    .consume_lifecycle(Json(TerminalSignal {
                        link_id: query.link_id,
                        request_id: query.request_id,
                        revision: request.revision,
                        decision_id: decision.decision_id.clone(),
                    }))
                    .call()
                    .await?;
                return Ok(Json(WorkflowResult { state, dispatch }));
            }
            let deadline = request
                .decision_deadline
                .ok_or_else(|| TerminalError::new_with_code(500, "pending deadline missing"))?;
            let Json(wake_at) = ctx
                .run(|| async {
                    #[cfg(feature = "integration")]
                    if let Some(faults) = &self.faults {
                        use std::sync::atomic::Ordering;
                        if faults
                            .early_wakes
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                            .is_ok()
                        {
                            return Ok::<_, HandlerError>(Json(
                                deadline
                                    .min(chrono::Utc::now() + chrono::Duration::milliseconds(10)),
                            ));
                        }
                    }
                    Ok::<_, HandlerError>(Json(deadline))
                })
                .name("remaining_absolute_deadline")
                .await?;
            #[cfg(feature = "integration")]
            if let Some(faults) = &self.faults {
                ctx.run(|| async {
                    use std::sync::atomic::Ordering;
                    faults.waits.fetch_add(1, Ordering::SeqCst);
                    if faults.interrupt_timer.load(Ordering::SeqCst) {
                        if faults.timer_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Err(
                                std::io::Error::other("fixture: interrupted timer setup").into()
                            );
                        }
                        std::future::pending::<()>().await;
                    }
                    Ok::<_, HandlerError>(())
                })
                .name("before-sleep")
                .retry_policy(
                    restate_sdk::context::RunRetryPolicy::default()
                        .initial_delay(std::time::Duration::from_millis(100)),
                )
                .await?;
            }
            // SDK 0.10 sleep journals an absolute wake timestamp and compares
            // only the command name on replay (shared-core Sleep header_eq).
            // Recompute the relative argument immediately at syscall creation:
            // an unjournaled sleep must not reuse a pre-interruption duration.
            // Never branch on this nonjournaled clock; emit the same sleep even
            // when overdue. Already-journaled sleeps keep their original target.
            let wait = (wake_at - chrono::Utc::now()).to_std().unwrap_or_default();
            let timer = ctx.sleep(wait);
            // Once a signal has been consumed, an untrusted/early signal cannot
            // create a hot loop on an already-resolved promise.
            if signalled {
                timer.await?;
            } else {
                restate_sdk::select! {
                    signal = ctx.promise::<Json<TerminalSignal>>("terminal") => { signal?; signalled = true; },
                    result = timer => { result?; },
                }
            }
            // Early or delayed wakes re-enter the exclusive deadline check.
        }
    }

    async fn notify(
        &self,
        ctx: SharedWorkflowContext<'_>,
        Json(signal): Json<TerminalSignal>,
    ) -> Result<(), TerminalError> {
        if ctx.key() != signal.request_id.to_string()
            || signal.decision_id.is_empty()
            || signal.revision == 0
        {
            return Err(TerminalError::new_with_code(400, "invalid terminal signal"));
        }
        if let Some(Json(previous)) = ctx.peek_promise::<Json<TerminalSignal>>("terminal").await? {
            if previous != signal {
                return Err(TerminalError::new_with_code(
                    409,
                    "conflicting terminal signal",
                ));
            }
            return Ok(());
        }
        if !ctx
            .object_client::<InvitationLinkClient>(signal.link_id.to_string())
            .notification_needed(Json(signal.clone()))
            .call()
            .await?
            .0
        {
            return Ok(());
        }
        // Private ingress, a single link writer. peek is not a shared-handler CAS.
        ctx.resolve_promise("terminal", Json(signal));
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults {
            ctx.run(|| async {
                use std::sync::atomic::Ordering;
                if faults.interrupt_notification.load(Ordering::SeqCst) {
                    if faults.notification_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(
                            std::io::Error::other("fixture: interrupted notification").into()
                        );
                    }
                    std::future::pending::<()>().await;
                }
                Ok::<_, HandlerError>(())
            })
            .name("notification-after-resolve")
            .retry_policy(
                restate_sdk::context::RunRetryPolicy::default()
                    .initial_delay(std::time::Duration::from_millis(100)),
            )
            .await?;
        }
        Ok(())
    }
}
