//! Persistent request authority. Calls never synchronously return to the link.
use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::request_lifecycle::{
    ApprovedDispatch, DecideRequest, DecisionAction, DecisionOutcome, DecisionReceipt,
    DeliveryStatus, InitializeRequest, RequestSnapshot, RequestStatus, SubmittedCommand,
};
use ghinvite_core::storage::projection::{AuditIntent, RequestProjectionEnvelope};
use ghinvite_core::{RequestId, RequestState, admission::Admit};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture, SharedObjectContext,
    },
    endpoint::Builder,
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct InvitationRequest {
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<Faults>>,
}

/// A link-side caller for the focused initialization/eligibility fault matrix.
/// Only protocol tests bind it; assembled journeys use the real link owner.
#[cfg(feature = "integration")]
pub struct AdmissionProtocol;

#[cfg(feature = "integration")]
#[restate_sdk::object]
impl AdmissionProtocol {
    #[handler]
    async fn initialize(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InitializeRequest>,
    ) -> Result<(), TerminalError> {
        ctx.object_client::<InvitationRequestClient>(input.request.request_id.to_string())
            .initialize(Json(input))
            .call()
            .await
    }

    #[handler]
    async fn eligibility(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<EligibilityQuery>,
    ) -> Result<Json<EligibilityVerdict>, TerminalError> {
        ctx.object_client::<InvitationRequestClient>(input.request_id.to_string())
            .eligibility(Json(input))
            .call()
            .await
    }
}

#[cfg(feature = "integration")]
#[derive(Default)]
pub struct Faults {
    pub stage: std::sync::Mutex<Option<String>>,
    pub reached: std::sync::atomic::AtomicUsize,
    pub pause_after_send: std::sync::atomic::AtomicBool,
    pub sent_before_pause: std::sync::atomic::AtomicUsize,
    pub pause_before_dispatch: std::sync::atomic::AtomicBool,
    pub clock: std::sync::Mutex<Option<DateTime<Utc>>>,
    pub unavailable: std::sync::atomic::AtomicBool,
}

pub fn bind(builder: Builder) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest::default()
            .into_service_definition()
            .options(restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true)),
    )
}

#[cfg(feature = "integration")]
pub fn bind_with_faults(builder: Builder, faults: std::sync::Arc<Faults>) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest {
            faults: Some(faults),
        }
        .into_service_definition()
        .options(
            restate_sdk::endpoint::ServiceOptions::new()
                .enable_lazy_state(true)
                .journal_retention(std::time::Duration::from_secs(2))
                .idempotency_retention(std::time::Duration::from_secs(2)),
        ),
    )
}

impl InvitationRequest {
    fn now(&self) -> DateTime<Utc> {
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults
            && let Some(now) = *faults.clock.lock().unwrap()
        {
            return now;
        }
        Utc::now()
    }

    async fn evaluated_at(&self, ctx: &ObjectContext<'_>) -> Result<DateTime<Utc>, TerminalError> {
        Ok(ctx
            .run(|| async { Ok::<_, HandlerError>(Json(self.now())) })
            .name("evaluation_time")
            .await?
            .0)
    }
    async fn checkpoint(
        &self,
        ctx: &ObjectContext<'_>,
        stage: &'static str,
    ) -> Result<(), TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults {
            ctx.run(|| async {
                if faults.stage.lock().unwrap().as_deref() == Some(stage) {
                    faults
                        .reached
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Err(
                        std::io::Error::other("fixture: request boundary interrupted").into(),
                    );
                }
                Ok::<_, HandlerError>(())
            })
            .name(stage)
            .retry_policy(
                restate_sdk::context::RunRetryPolicy::default()
                    .initial_delay(std::time::Duration::from_millis(50)),
            )
            .await?;
        }
        let _ = (ctx, stage);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EligibilityQuery {
    pub request_id: RequestId,
    pub attempt: Admit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EligibilityVerdict {
    pub evaluated_at: DateTime<Utc>,
    pub state: RequestState,
    pub blocking: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct VerdictRecord {
    input: EligibilityQuery,
    verdict: EligibilityVerdict,
}

fn missing() -> TerminalError {
    TerminalError::new_with_code(404, "not found")
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "operation conflict")
}

async fn request(ctx: &ObjectContext<'_>) -> Result<RequestSnapshot, TerminalError> {
    Ok(ctx
        .get::<Json<RequestSnapshot>>("request")
        .await?
        .ok_or_else(missing)?
        .0)
}

async fn expire(
    ctx: &ObjectContext<'_>,
    mut request: RequestSnapshot,
    now: DateTime<Utc>,
) -> RequestSnapshot {
    if expire_due(&mut request, now) {
        ctx.set("request", Json(request.clone()));
    }
    request
}

/// One deadline transition, shared by inspection, timers and decision arbitration.
fn expire_due(request: &mut RequestSnapshot, now: DateTime<Utc>) -> bool {
    if request.state == RequestState::Pending && request.decision_deadline.is_some_and(|d| now >= d)
    {
        request.state = RequestState::Expired;
        request.revision += 1;
        request.decision = Some(ghinvite_core::request_lifecycle::TerminalDecision {
            decision_id: format!("request.expired/{}", request.request_id),
            decided_by: None,
            effective_at: request.decision_deadline.unwrap(),
            evaluated_at: now,
            decline_reason: None,
        });
        true
    } else {
        false
    }
}

fn events(request: &RequestSnapshot) -> Vec<AuditIntent> {
    let mut events = vec![AuditIntent {
        event_id: format!("request.created/{}", request.request_id),
        kind: EventType::RequestCreated,
        actor_id: Some(request.requester_id),
        target_id: request.request_id.to_string(),
        effective_at: request.admitted_at,
        evaluated_at: request.admitted_at,
    }];
    if let Some(decision) = &request.decision {
        events.push(AuditIntent {
            event_id: decision.decision_id.clone(),
            kind: match request.state {
                RequestState::Approved => EventType::RequestApproved,
                RequestState::Declined => EventType::RequestDeclined,
                _ => EventType::RequestExpired,
            },
            actor_id: decision.decided_by,
            target_id: request.request_id.to_string(),
            effective_at: decision.effective_at,
            evaluated_at: decision.evaluated_at,
        });
    }
    events
}

async fn project(ctx: &ObjectContext<'_>, request: &RequestSnapshot) -> Result<(), TerminalError> {
    ctx.object_client::<crate::projection::RequestProjectionClient>(ctx.key())
        .apply(Json(RequestProjectionEnvelope {
            request: request.clone(),
            events: events(request),
        }))
        .send()
        .await?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct DecisionRecord {
    input: DecideRequest,
    receipt: DecisionReceipt,
}

fn normalize(input: &mut DecideRequest) -> Result<String, TerminalError> {
    if let DecisionAction::Decline { reason } = &mut input.action {
        *reason = reason
            .take()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if reason.as_ref().is_some_and(|s| s.len() > 16_384) {
            return Err(TerminalError::new_with_code(400, "invalid reason"));
        }
    }
    Ok(format!(
        "decision/{}",
        String::from(input.operation_id.clone())
    ))
}

fn admin(request: &RequestSnapshot, input: &DecideRequest) -> Result<(), TerminalError> {
    if input.request_id != request.request_id
        || input.link_id != request.link_id
        || input.admin.account_id != request.account_id
        || input.admin.user_id == 0
    {
        return Err(missing());
    }
    Ok(())
}

impl InvitationRequest {
    async fn retain_plan(
        &self,
        ctx: &ObjectContext<'_>,
        request: &RequestSnapshot,
    ) -> Result<(), TerminalError> {
        if request.state != RequestState::Approved
            || ctx.get::<Json<ApprovedDispatch>>("plan").await?.is_some()
        {
            return Ok(());
        }
        let mut input = ctx
            .get::<Json<InitializeRequest>>("input")
            .await?
            .ok_or_else(missing)?
            .0;
        input.request = request.clone();
        let Json(plan) = ctx
            .run(|| async {
                let decision = request.decision.as_ref().ok_or_else(conflict)?;
                let commands = input
                    .repos
                    .iter()
                    .map(|repo| ghinvite_core::delivery::CreateCommand {
                        invitation_id: ghinvite_core::GithubInvitationId::new(),
                        link_id: request.link_id,
                        request_id: request.request_id,
                        approval_id: decision.decision_id.clone(),
                        account_id: request.account_id,
                        installation_id: input.installation_id,
                        requester_id: request.requester_id,
                        repo_id: repo.repo_id,
                        repo_full_name: repo.repo_full_name.clone(),
                        permission: input.permission,
                        approved_at: decision.effective_at,
                    })
                    .collect();
                Ok::<_, HandlerError>(Json(ApprovedDispatch {
                    dispatch_id: format!("dispatch/{}", request.request_id),
                    input: input.clone(),
                    commands,
                }))
            })
            .name("approved_manifest")
            .await?;
        ctx.set("plan", Json(plan));
        Ok(())
    }

    async fn arrange(
        &self,
        ctx: &ObjectContext<'_>,
        request: &RequestSnapshot,
    ) -> Result<(), TerminalError> {
        self.retain_plan(ctx, request).await?;
        project(ctx, request).await?;
        if request.state == RequestState::Approved {
            ctx.object_client::<InvitationRequestClient>(ctx.key())
                .dispatch()
                .send()
                .await?;
        }
        Ok(())
    }
}

#[restate_sdk::object]
impl InvitationRequest {
    #[handler]
    async fn initialize(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InitializeRequest>,
    ) -> Result<(), TerminalError> {
        #[cfg(feature = "integration")]
        if self
            .faults
            .as_ref()
            .is_some_and(|f| f.unavailable.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(TerminalError::new_with_code(
                503,
                "fixture: owner unavailable",
            ));
        }
        if ctx.key() != input.request.request_id.to_string() {
            return Err(missing());
        }
        let initial = &input.request;
        let valid_policy = if input.approval_required {
            initial.state == RequestState::Pending
                && initial.decision.is_none()
                && initial.decision_deadline
                    == Some(initial.admitted_at + crate::admission::PENDING_LIFETIME)
        } else {
            initial.state == RequestState::Approved
                && initial.decision_deadline.is_none()
                && initial.decision.as_ref().is_some_and(|d| {
                    d.decided_by.is_none()
                        && d.effective_at == initial.admitted_at
                        && d.evaluated_at == initial.admitted_at
                        && d.decline_reason.is_none()
                        && d.decision_id == format!("request.approved/{}", initial.request_id)
                })
        };
        if !valid_policy
            || initial.revision != 1
            || initial.requester_id == 0
            || initial.account_id == 0
            || input.installation_id == 0
            || ghinvite_core::RepositoryScope::parse(input.repos.clone()).is_err()
        {
            return Err(TerminalError::new_with_code(
                400,
                "invalid admission context",
            ));
        }
        if let Some(Json(old)) = ctx.get::<Json<InitializeRequest>>("input").await? {
            if old != input {
                return Err(conflict());
            }
            if ctx.get::<bool>("initialized").await?.unwrap_or(false) {
                return Ok(());
            }
        } else {
            ctx.set("input", Json(input.clone()));
            ctx.set("request", Json(input.request.clone()));
        }
        self.checkpoint(&ctx, "initialization-recorded").await?;
        self.arrange(&ctx, &request(&ctx).await?).await?;
        if let Some(deadline) = input.request.decision_deadline {
            ctx.object_client::<InvitationRequestClient>(ctx.key())
                .tick_expire()
                .send_after((deadline - Utc::now()).to_std().unwrap_or_default())
                .await?;
        }
        ctx.set("initialized", true);
        Ok(())
    }

    #[handler]
    async fn eligibility(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<EligibilityQuery>,
    ) -> Result<Json<EligibilityVerdict>, TerminalError> {
        #[cfg(feature = "integration")]
        if self
            .faults
            .as_ref()
            .is_some_and(|f| f.unavailable.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(TerminalError::new_with_code(
                503,
                "fixture: owner unavailable",
            ));
        }
        input
            .attempt
            .normalize()
            .map_err(|_| TerminalError::new_with_code(400, "invalid attempt"))?;
        let request = request(&ctx).await?;
        if input.request_id != request.request_id
            || input.attempt.link_id != request.link_id
            || input.attempt.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let key = format!(
            "verdict/{}",
            String::from(input.attempt.operation_id.clone())
        );
        if let Some(Json(old)) = ctx.get::<Json<VerdictRecord>>(&key).await? {
            return if old.input == input {
                Ok(Json(old.verdict))
            } else {
                Err(conflict())
            };
        }
        let evaluated_at = self.evaluated_at(&ctx).await?;
        let revision = request.revision;
        let request = expire(&ctx, request, evaluated_at).await;
        if revision != request.revision {
            project(&ctx, &request).await?;
        }
        let verdict = EligibilityVerdict {
            evaluated_at,
            state: request.state,
            blocking: matches!(
                request.state,
                RequestState::Pending | RequestState::Approved
            ),
        };
        ctx.set(
            &key,
            Json(VerdictRecord {
                input,
                verdict: verdict.clone(),
            }),
        );
        self.checkpoint(&ctx, "verdict-recorded").await?;
        Ok(Json(verdict))
    }

    #[handler]
    async fn admin_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::request_lifecycle::AdminRequestStatus>,
    ) -> Result<Json<RequestSnapshot>, TerminalError> {
        let request = request(&ctx).await?;
        if input.request_id != request.request_id
            || input.admin.account_id != request.account_id
            || input.admin.user_id == 0
        {
            return Err(missing());
        }
        let request = expire(&ctx, request, self.evaluated_at(&ctx).await?).await;
        project(&ctx, &request).await?;
        Ok(Json(request))
    }

    #[handler]
    async fn request_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RequestStatus>,
    ) -> Result<Json<RequestSnapshot>, TerminalError> {
        let request = request(&ctx).await?;
        if input.request_id != request.request_id
            || input.link_id != request.link_id
            || input.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let mut request = expire(&ctx, request, self.evaluated_at(&ctx).await?).await;
        project(&ctx, &request).await?;
        if let Some(decision) = &mut request.decision {
            decision.decline_reason = None;
        }
        Ok(Json(request))
    }

    #[handler]
    async fn tick_expire(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        let request = expire(&ctx, request(&ctx).await?, self.evaluated_at(&ctx).await?).await;
        project(&ctx, &request).await?;
        if request.state == RequestState::Pending {
            ctx.object_client::<InvitationRequestClient>(ctx.key())
                .tick_expire()
                .send_after(
                    (request.decision_deadline.ok_or_else(conflict)? - Utc::now())
                        .to_std()
                        .unwrap_or_default(),
                )
                .await?;
        }
        Ok(())
    }

    #[handler]
    async fn decision_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<DecideRequest>,
    ) -> Result<Json<Option<DecisionReceipt>>, TerminalError> {
        admin(&request(&ctx).await?, &input)?;
        let key = normalize(&mut input)?;
        match ctx.get::<Json<DecisionRecord>>(&key).await? {
            Some(Json(old)) if old.input == input => Ok(Json(Some(old.receipt))),
            Some(_) => Err(conflict()),
            None => Ok(Json(None)),
        }
    }

    #[handler]
    async fn decide(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<DecideRequest>,
    ) -> Result<Json<DecisionReceipt>, TerminalError> {
        let request = request(&ctx).await?;
        admin(&request, &input)?;
        let key = normalize(&mut input)?;
        if let Some(Json(old)) = ctx.get::<Json<DecisionRecord>>(&key).await? {
            return if old.input == input {
                Ok(Json(old.receipt))
            } else {
                Err(conflict())
            };
        }
        self.checkpoint(&ctx, "before-decision").await?;
        let Json(receipt) = ctx
            .run(|| async {
                let mut request = request.clone();
                let now = self.now();
                let was_pending = request.state == RequestState::Pending;
                if was_pending && !expire_due(&mut request, now) {
                    request.decision_deadline.ok_or_else(conflict)?;
                    let (state, kind, reason, actor, effective_at) = match &input.action {
                        DecisionAction::Approve => (
                            RequestState::Approved,
                            EventType::RequestApproved,
                            None,
                            Some(input.admin.user_id),
                            now,
                        ),
                        DecisionAction::Decline { reason } => (
                            RequestState::Declined,
                            EventType::RequestDeclined,
                            reason.clone(),
                            Some(input.admin.user_id),
                            now,
                        ),
                    };
                    request.state = state;
                    request.revision += 1;
                    request.decision = Some(ghinvite_core::request_lifecycle::TerminalDecision {
                        decision_id: format!("{kind}/{}", request.request_id),
                        decided_by: actor,
                        effective_at,
                        evaluated_at: now,
                        decline_reason: reason,
                    });
                }
                let matching = match &input.action {
                    DecisionAction::Approve => request.state == RequestState::Approved,
                    DecisionAction::Decline { reason } => {
                        request.state == RequestState::Declined
                            && request
                                .decision
                                .as_ref()
                                .is_some_and(|d| &d.decline_reason == reason)
                    }
                };
                let outcome = if !matching {
                    DecisionOutcome::Incompatible
                } else if was_pending {
                    DecisionOutcome::Applied
                } else {
                    DecisionOutcome::AlreadyCompleted
                };
                Ok::<_, HandlerError>(Json(DecisionReceipt { request, outcome }))
            })
            .name("record_decision")
            .await?;
        self.checkpoint(&ctx, "after-decision").await?;
        ctx.set("request", Json(receipt.request.clone()));
        self.arrange(&ctx, &receipt.request).await?;
        ctx.set(
            &key,
            Json(DecisionRecord {
                input,
                receipt: receipt.clone(),
            }),
        );
        Ok(Json(receipt))
    }

    #[handler]
    async fn dispatch(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults {
            ctx.run(|| async {
                if faults
                    .pause_before_dispatch
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(std::io::Error::other("fixture: dispatch paused").into());
                }
                Ok::<_, HandlerError>(())
            })
            .name("before_dispatch")
            .retry_policy(
                restate_sdk::context::RunRetryPolicy::default()
                    .initial_delay(std::time::Duration::from_millis(50)),
            )
            .await?;
        }
        let Json(plan) = ctx
            .get::<Json<ApprovedDispatch>>("plan")
            .await?
            .ok_or_else(missing)?;
        for command in &plan.commands {
            let key = format!("submitted/{}", command.repo_id);
            if ctx.get::<Json<SubmittedCommand>>(&key).await?.is_some() {
                continue;
            }
            let invocation_id = ctx
                .object_client::<crate::delivery::RepositoryDeliveryClient>(command.delivery_key())
                .create(Json(command.clone()))
                .send()
                .await?
                .invocation_id()
                .to_owned();
            self.checkpoint(&ctx, "after-repository-send").await?;
            #[cfg(feature = "integration")]
            if let Some(faults) = &self.faults {
                ctx.run(|| async {
                    if faults
                        .pause_after_send
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        faults
                            .sent_before_pause
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return Err(std::io::Error::other(
                            "fixture: send recorded before progress",
                        )
                        .into());
                    }
                    Ok::<_, HandlerError>(())
                })
                .name("after_send")
                .retry_policy(
                    restate_sdk::context::RunRetryPolicy::default()
                        .initial_delay(std::time::Duration::from_millis(50)),
                )
                .await?;
            }
            ctx.set(
                &key,
                Json(SubmittedCommand {
                    command: command.clone(),
                    invocation_id,
                }),
            );
        }
        Ok(())
    }

    #[handler]
    async fn recover(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<DeliveryStatus>, TerminalError> {
        let request = request(&ctx).await?;
        if query.request_id != request.request_id
            || query.link_id != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let Json(plan) = ctx
            .get::<Json<ApprovedDispatch>>("plan")
            .await?
            .ok_or_else(missing)?;
        let mut submitted = Vec::new();
        for command in &plan.commands {
            let invocation_id = ctx
                .object_client::<crate::delivery::RepositoryDeliveryClient>(command.delivery_key())
                .create(Json(command.clone()))
                .send()
                .await?
                .invocation_id()
                .to_owned();
            let record = SubmittedCommand {
                command: command.clone(),
                invocation_id,
            };
            ctx.set(
                &format!("submitted/{}", command.repo_id),
                Json(record.clone()),
            );
            submitted.push(record);
        }
        Ok(Json(DeliveryStatus { plan, submitted }))
    }

    #[handler]
    async fn delivery_status(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<DeliveryStatus>, TerminalError> {
        let request = ctx
            .get::<Json<RequestSnapshot>>("request")
            .await?
            .ok_or_else(missing)?
            .0;
        if query.request_id != request.request_id
            || query.link_id != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let Json(plan) = ctx
            .get::<Json<ApprovedDispatch>>("plan")
            .await?
            .ok_or_else(missing)?;
        let mut submitted = Vec::new();
        for command in &plan.commands {
            if let Some(Json(record)) = ctx
                .get::<Json<SubmittedCommand>>(&format!("submitted/{}", command.repo_id))
                .await?
            {
                submitted.push(record);
            }
        }
        Ok(Json(DeliveryStatus { plan, submitted }))
    }

    #[handler]
    async fn delivery_progress(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<Vec<ghinvite_core::delivery::RepositoryProgress>>, TerminalError> {
        use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
        let request = ctx
            .get::<Json<RequestSnapshot>>("request")
            .await?
            .ok_or_else(missing)?
            .0;
        if query.request_id != request.request_id
            || query.link_id != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let Some(Json(plan)) = ctx.get::<Json<ApprovedDispatch>>("plan").await? else {
            return Ok(Json(Vec::new()));
        };
        let mut result = Vec::new();
        for command in plan.commands {
            result.push(RepositoryProgress {
                repo_id: command.repo_id,
                stage: if ctx
                    .get::<Json<SubmittedCommand>>(&format!("submitted/{}", command.repo_id))
                    .await?
                    .is_some()
                {
                    DispatchStage::Submitted
                } else {
                    DispatchStage::Planned
                },
            });
        }
        Ok(Json(result))
    }

    #[handler]
    async fn approved_plan(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<ApprovedDispatch>, TerminalError> {
        let request = ctx
            .get::<Json<RequestSnapshot>>("request")
            .await?
            .ok_or_else(missing)?
            .0;
        if query.request_id != request.request_id
            || query.link_id != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        ctx.get("plan").await?.ok_or_else(missing)
    }
}
