//! Link-owned guardrails, use consumption, immutable admission receipts and indexes.
//! Private ingress: identities assert the authenticated caller's current authority.
use crate::request_owner::{EligibilityQuery, InvitationRequestClient};
use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::{InvitationLinkId, RequestId, RequestState};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    endpoint::{Builder, ServiceOptions},
    errors::{HandlerError, TerminalError},
    serde::Json,
    service::IntoServiceDefinition,
};
use serde::{Deserialize, Serialize};

pub use ghinvite_core::admission::*;
pub use ghinvite_core::request_lifecycle::{
    DecideRequest, DecisionAction, DecisionOutcome, DecisionReceipt, InitializeRequest,
    RequestSnapshot, RequestStatus, TerminalDecision,
};
pub use ghinvite_core::storage::projection::{
    AccountAdmin, AuditIntent, CreateLink, LinkSnapshot, ProjectionEnvelope,
};
pub const PENDING_LIFETIME: chrono::Duration = chrono::Duration::days(7);

#[derive(Clone, Serialize, Deserialize)]
struct OperationRecord {
    input: Admit,
    receipt: AdmissionReceipt,
}

#[derive(Clone, Serialize, Deserialize)]
struct AdmissionDecision {
    operation: OperationRecord,
    link: Option<LinkSnapshot>,
    initialization: Option<InitializeRequest>,
}

#[derive(Default)]
pub struct InvitationLink {
    #[cfg(feature = "integration")]
    skip_availability: bool,
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<Faults>>,
}

#[cfg(feature = "integration")]
#[derive(Default)]
pub struct Faults {
    stage: std::sync::Mutex<Option<String>>,
    attempts: std::sync::atomic::AtomicUsize,
    clock: std::sync::Mutex<Option<DateTime<Utc>>>,
}
#[cfg(feature = "integration")]
impl Faults {
    pub fn set_clock(&self, now: Option<DateTime<Utc>>) {
        *self.clock.lock().unwrap() = now;
    }
    pub fn arm(&self, stage: &str) {
        self.attempts.store(0, std::sync::atomic::Ordering::SeqCst);
        *self.stage.lock().unwrap() = Some(stage.into());
    }
    pub fn attempts(&self) -> usize {
        self.attempts.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn release(&self) {
        *self.stage.lock().unwrap() = None;
    }
}

pub fn bind(builder: Builder) -> Builder {
    builder.bind(
        InvitationLink::default()
            .into_service_definition()
            .options(ServiceOptions::new().enable_lazy_state(true)),
    )
}
#[cfg(feature = "integration")]
pub fn bind_with_faults(builder: Builder, faults: std::sync::Arc<Faults>) -> Builder {
    builder.bind(
        InvitationLink {
            faults: Some(faults),
            skip_availability: true,
        }
        .into_service_definition()
        .options(
            ServiceOptions::new()
                .enable_lazy_state(true)
                .idempotency_retention(std::time::Duration::from_secs(2))
                .journal_retention(std::time::Duration::from_secs(2)),
        ),
    )
}
#[cfg(feature = "integration")]
pub fn bind_protocol_fixture(builder: Builder) -> Builder {
    builder.bind(
        InvitationLink {
            faults: None,
            skip_availability: true,
        }
        .into_service_definition()
        .options(ServiceOptions::new().enable_lazy_state(true)),
    )
}

fn missing() -> TerminalError {
    TerminalError::new_with_code(404, "not found")
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "operation conflict")
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid command")
}
fn uncertain() -> TerminalError {
    TerminalError::new_with_code(503, "Authority unavailable. Retry the same attempt.")
}
fn operation_key(id: &AdmissionOperationId) -> String {
    format!("op/{}", String::from(id.clone()))
}
fn attempt_key(id: &AdmissionOperationId) -> String {
    format!("attempt/{}", String::from(id.clone()))
}
fn latest(requester: u64) -> String {
    format!("latest-attempt/{requester}")
}
fn pointer(requester: u64) -> String {
    format!("latest-request/{requester}")
}
fn validate_key(ctx: &ObjectContext<'_>, id: InvitationLinkId) -> Result<(), TerminalError> {
    if ctx.key() != id.to_string() {
        return Err(missing());
    }
    Ok(())
}
fn validate_admin(admin: &AccountAdmin, account: u64) -> Result<(), TerminalError> {
    if admin.account_id != account || account == 0 || admin.user_id == 0 {
        return Err(missing());
    }
    Ok(())
}
async fn link(ctx: &ObjectContext<'_>) -> Result<LinkSnapshot, TerminalError> {
    Ok(ctx
        .get::<Json<LinkSnapshot>>("link")
        .await?
        .ok_or_else(missing)?
        .0)
}
async fn admin_link(
    ctx: &ObjectContext<'_>,
    id: InvitationLinkId,
    admin: &AccountAdmin,
) -> Result<LinkSnapshot, TerminalError> {
    validate_key(ctx, id)?;
    let link = link(ctx).await?;
    validate_admin(admin, link.creation.account_id)?;
    Ok(link)
}
fn event(kind: EventType, id: impl ToString, actor: Option<u64>, at: DateTime<Utc>) -> AuditIntent {
    AuditIntent {
        event_id: format!("{kind}/{}", id.to_string()),
        kind,
        actor_id: actor,
        target_id: id.to_string(),
        effective_at: at,
        evaluated_at: at,
    }
}
async fn project(
    ctx: &ObjectContext<'_>,
    link: &LinkSnapshot,
    events: Vec<AuditIntent>,
) -> Result<(), TerminalError> {
    ctx.object_client::<crate::projection::InvitationProjectionClient>(ctx.key())
        .apply_transition(Json(ProjectionEnvelope {
            transition_id: format!("link/{}/{}", link.link_id, link.revision),
            link: link.clone(),
            requests: vec![],
            events,
        }))
        .send()
        .await?;
    Ok(())
}

impl InvitationLink {
    fn now(&self) -> DateTime<Utc> {
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults
            && let Some(now) = *faults.clock.lock().unwrap()
        {
            return now;
        }
        Utc::now()
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
                        .attempts
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Err(std::io::Error::other("fixture: interrupted link boundary").into());
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
    async fn status(
        &self,
        ctx: &ObjectContext<'_>,
        request_id: RequestId,
        requester_id: u64,
        link_id: InvitationLinkId,
    ) -> Result<RequestSnapshot, TerminalError> {
        ctx.object_client::<InvitationRequestClient>(request_id.to_string())
            .request_status(Json(RequestStatus {
                request_id,
                requester_id,
                link_id,
            }))
            .call()
            .await
            .map(|Json(request)| request)
            .map_err(|_| uncertain())
    }
}

#[restate_sdk::object]
impl InvitationLink {
    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<CreateLink>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id)?;
        validate_admin(&input.admin, input.account_id)?;
        let input = input.normalized().map_err(|_| invalid())?;
        if let Some(Json(old)) = ctx.get::<Json<LinkSnapshot>>("creation").await? {
            return if old.creation == input {
                Ok(Json(old))
            } else {
                Err(conflict())
            };
        }
        self.checkpoint(&ctx, "creation-before-decision").await?;
        let Json(link) = ctx
            .run(|| async {
                let now = self.now();
                if input.expires_at.is_some_and(|expiry| expiry <= now) {
                    return Err(HandlerError::from(invalid()));
                }
                Ok::<_, HandlerError>(Json(LinkSnapshot {
                    metadata: None,
                    link_id: input.link_id,
                    creation: input.clone(),
                    created_at: now,
                    uses: 0,
                    revision: 1,
                    revoked_at: None,
                    revoked_by: None,
                }))
            })
            .name("decide_creation")
            .await?;
        self.checkpoint(&ctx, "creation-after-decision").await?;
        ctx.set("link", Json(link.clone()));
        self.checkpoint(&ctx, "creation-after-link").await?;
        ctx.set("creation", Json(link.clone()));
        self.checkpoint(&ctx, "creation-after-receipt").await?;
        project(
            &ctx,
            &link,
            vec![event(
                EventType::InvitationLinkCreated,
                link.link_id,
                Some(input.admin.user_id),
                link.created_at,
            )],
        )
        .await?;
        self.checkpoint(&ctx, "creation-after-projection-send")
            .await?;
        Ok(Json(link))
    }

    #[handler]
    async fn link_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        Ok(Json(admin_link(&ctx, input.link_id, &input.admin).await?))
    }

    #[handler]
    async fn revoke(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        let link = admin_link(&ctx, input.link_id, &input.admin).await?;
        if link.revoked_at.is_some() {
            return Ok(Json(link));
        }
        let Json(link) = ctx
            .run(|| async {
                let mut link = link.clone();
                link.revoked_at = Some(self.now());
                link.revoked_by = Some(input.admin.user_id);
                link.revision += 1;
                Ok::<_, HandlerError>(Json(link))
            })
            .name("decide_revocation")
            .await?;
        ctx.set("link", Json(link.clone()));
        project(
            &ctx,
            &link,
            vec![event(
                EventType::InvitationLinkRevoked,
                link.link_id,
                link.revoked_by,
                link.revoked_at.unwrap(),
            )],
        )
        .await?;
        Ok(Json(link))
    }

    #[handler]
    async fn update_metadata(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<UpdateMetadata>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        let mut link = admin_link(&ctx, input.link_id, &input.admin).await?;
        let metadata = ghinvite_core::storage::projection::LinkMetadata::parse(
            &input.description,
            input.internal_note.as_deref(),
        )
        .map_err(|_| invalid())?;
        if link.description() == metadata.description
            && link.internal_note() == metadata.internal_note.as_deref()
        {
            return Ok(Json(link));
        }
        link.metadata = Some(metadata);
        link.revision += 1;
        let Json(mut event) = ctx
            .run(|| async {
                Ok::<_, HandlerError>(Json(event(
                    EventType::InvitationLinkMetadataUpdated,
                    link.link_id,
                    Some(input.admin.user_id),
                    self.now(),
                )))
            })
            .name("metadata_event")
            .await?;
        event.event_id = format!("link/{}/metadata/{}", link.link_id, link.revision);
        ctx.set("link", Json(link.clone()));
        project(&ctx, &link, vec![event]).await?;
        Ok(Json(link))
    }

    #[handler]
    async fn prepare_attempt(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<Admit>,
    ) -> Result<Json<Attempt>, TerminalError> {
        validate_key(&ctx, input.link_id)?;
        input.normalize().map_err(|_| invalid())?;
        if input.requester_id == 0 {
            return Err(missing());
        }
        link(&ctx).await?;
        if let Some(Json(old)) = ctx
            .get::<Json<OperationRecord>>(&operation_key(&input.operation_id))
            .await?
        {
            if old.input != input {
                return Err(conflict());
            }
            return Ok(Json(Attempt {
                input,
                receipt: Some(old.receipt),
            }));
        }
        let key = attempt_key(&input.operation_id);
        if let Some(Json(old)) = ctx.get::<Json<Admit>>(&key).await? {
            if old != input {
                return Err(conflict());
            }
        } else {
            ctx.set(&key, Json(input.clone()));
            ctx.set(
                &latest(input.requester_id),
                Json(input.operation_id.clone()),
            );
        }
        Ok(Json(Attempt {
            input,
            receipt: None,
        }))
    }

    #[handler]
    async fn admit(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<Admit>,
    ) -> Result<Json<AdmissionReceipt>, TerminalError> {
        validate_key(&ctx, input.link_id)?;
        if input.requester_id == 0 {
            return Err(missing());
        }
        input.normalize().map_err(|_| invalid())?;
        let key = operation_key(&input.operation_id);
        if let Some(Json(old)) = ctx.get::<Json<OperationRecord>>(&key).await? {
            return if old.input == input {
                Ok(Json(old.receipt))
            } else {
                Err(conflict())
            };
        }
        let handoff_key = format!("handoff/{}", String::from(input.operation_id.clone()));
        if let Some(Json(pending)) = ctx.get::<Json<AdmissionDecision>>(&handoff_key).await? {
            if pending.operation.input != input {
                return Err(conflict());
            }
            let initialization = pending.initialization.ok_or_else(uncertain)?;
            ctx.object_client::<InvitationRequestClient>(
                initialization.request.request_id.to_string(),
            )
            .initialize(Json(initialization))
            .call()
            .await
            .map_err(|_| uncertain())?;
            ctx.set(&key, Json(pending.operation.clone()));
            ctx.clear(&handoff_key);
            return Ok(Json(pending.operation.receipt));
        }
        let prepared = attempt_key(&input.operation_id);
        if let Some(Json(old)) = ctx.get::<Json<Admit>>(&prepared).await?
            && old != input
        {
            return Err(conflict());
        }
        ctx.set(&prepared, Json(input.clone()));
        ctx.set(
            &latest(input.requester_id),
            Json(input.operation_id.clone()),
        );
        let link = link(&ctx).await?;
        if let Some(Json(request_id)) = ctx
            .get::<Json<RequestId>>(&pointer(input.requester_id))
            .await?
        {
            let verdict = ctx
                .object_client::<InvitationRequestClient>(request_id.to_string())
                .eligibility(Json(EligibilityQuery {
                    request_id,
                    attempt: input.clone(),
                }))
                .call()
                .await
                .map_err(|_| uncertain())?
                .0;
            self.checkpoint(&ctx, "after-verdict").await?;
            if verdict.blocking {
                let receipt = AdmissionReceipt {
                    decided_at: verdict.evaluated_at,
                    result: AdmissionResult::Rejected {
                        reason: Rejection::ExistingRequest,
                    },
                };
                ctx.set(
                    &key,
                    Json(OperationRecord {
                        input,
                        receipt: receipt.clone(),
                    }),
                );
                return Ok(Json(receipt));
            }
        }
        self.checkpoint(&ctx, "before-decision").await?;
        let check = ctx
            .run(|| async { Ok::<_, HandlerError>(link.inactive(self.now()).is_none()) })
            .name("needs_availability")
            .await?;
        #[cfg(feature = "integration")]
        let check = check && !self.skip_availability;
        let evidence = if check {
            let response = ctx
                .object_client::<crate::availability::AccountInstallationClient>(
                    link.creation.account_id.to_string(),
                )
                .admission_evidence(Json(crate::availability::Scope {
                    account_id: link.creation.account_id,
                    repo_ids: link.creation.repos.iter().map(|r| r.repo_id).collect(),
                }))
                .call()
                .await
                .map_err(|_| uncertain())?;
            Some(response.0)
        } else {
            None
        };
        let Json(decision) = ctx
            .run(|| async {
                let now = self.now();
                // A call result can be replayed long after it was observed.
                // Leave the attempt undecided; a fresh invocation obtains new
                // evidence rather than converting stale evidence into a receipt.
                if link.inactive(now).is_none()
                    && evidence.as_ref().is_some_and(|e| now >= e.valid_until)
                {
                    return Err(HandlerError::from(uncertain()));
                }
                let eligibility = evidence
                    .as_ref()
                    .map(|e| &e.eligibility)
                    .unwrap_or(&crate::availability::Eligibility::Available);
                let reason =
                    link.inactive(now)
                        .map(Rejection::from)
                        .or_else(|| match &eligibility {
                            crate::availability::Eligibility::Unavailable { reason } => {
                                Some(reason.clone())
                            }
                            _ => None,
                        });
                let (result, changed, initialization) = if let Some(reason) = reason {
                    (AdmissionResult::Rejected { reason }, None, None)
                } else if matches!(eligibility, crate::availability::Eligibility::Unknown) {
                    return Err(HandlerError::from(uncertain()));
                } else {
                    let mut link = link.clone();
                    link.uses = link.uses.checked_add(1).ok_or_else(invalid)?;
                    link.revision += 1;
                    let request_id = RequestId::new();
                    let state = if link.creation.approval_required {
                        RequestState::Pending
                    } else {
                        RequestState::Approved
                    };
                    let deadline = link
                        .creation
                        .approval_required
                        .then_some(now + PENDING_LIFETIME);
                    let request = RequestSnapshot {
                        request_id,
                        link_id: link.link_id,
                        account_id: link.creation.account_id,
                        requester_id: input.requester_id,
                        justification: input.justification.clone(),
                        state,
                        admitted_at: now,
                        decision_deadline: deadline,
                        revision: 1,
                        decision: (state == RequestState::Approved).then(|| TerminalDecision {
                            decision_id: format!("request.approved/{request_id}"),
                            decided_by: None,
                            effective_at: now,
                            evaluated_at: now,
                            decline_reason: None,
                        }),
                    };
                    let initialization = InitializeRequest {
                        request,
                        installation_id: link.creation.installation_id,
                        repos: link.creation.repos.clone(),
                        permission: link.creation.permission,
                        approval_required: link.creation.approval_required,
                    };
                    (
                        AdmissionResult::Accepted {
                            request_id,
                            state,
                            decision_deadline: deadline,
                        },
                        Some(link),
                        Some(initialization),
                    )
                };
                Ok::<_, HandlerError>(Json(AdmissionDecision {
                    operation: OperationRecord {
                        input: input.clone(),
                        receipt: AdmissionReceipt {
                            decided_at: now,
                            result,
                        },
                    },
                    link: changed,
                    initialization,
                }))
            })
            .name("decide_admission")
            .await?;
        self.checkpoint(&ctx, "after-decision").await?;
        if let Some(link) = &decision.link {
            ctx.set("link", Json(link.clone()));
            self.checkpoint(&ctx, "after-link").await?;
            let events = if link
                .creation
                .max_uses
                .is_some_and(|max| link.uses == u64::from(max))
            {
                vec![event(
                    EventType::InvitationLinkExhausted,
                    link.link_id,
                    None,
                    decision.operation.receipt.decided_at,
                )]
            } else {
                vec![]
            };
            project(&ctx, link, events).await?;
        }
        if let Some(initialization) = decision.initialization.clone() {
            // Immutable context is retained with the link; it is never read as lifecycle state.
            ctx.set(
                &format!("admission/{}", initialization.request.request_id),
                Json(initialization.clone()),
            );
            ctx.set(
                &pointer(input.requester_id),
                Json(initialization.request.request_id),
            );
            ctx.set(&handoff_key, Json(decision.clone()));
            self.checkpoint(&ctx, "before-initialize").await?;
            ctx.object_client::<InvitationRequestClient>(
                initialization.request.request_id.to_string(),
            )
            .initialize(Json(initialization))
            .call()
            .await
            .map_err(|_| uncertain())?;
            self.checkpoint(&ctx, "after-initialize").await?;
        }
        ctx.set(&key, Json(decision.operation.clone()));
        ctx.clear(&handoff_key);
        self.checkpoint(&ctx, "after-outcome").await?;
        Ok(Json(decision.operation.receipt))
    }

    #[handler]
    async fn requester_page(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<AttemptQuery>,
    ) -> Result<Json<RequesterPage>, TerminalError> {
        validate_key(&ctx, query.link_id)?;
        if query.requester_id == 0 {
            return Err(missing());
        }
        let link = link(&ctx).await?;
        let explicit = query.operation_id.is_some();
        let operation_id = match query.operation_id {
            Some(id) => Some(id),
            None => ctx
                .get::<Json<AdmissionOperationId>>(&latest(query.requester_id))
                .await?
                .map(|v| v.0),
        };
        let mut attempt = None;
        if let Some(id) = operation_id {
            if let Some(Json(old)) = ctx
                .get::<Json<OperationRecord>>(&operation_key(&id))
                .await?
            {
                attempt = Some(Attempt {
                    input: old.input,
                    receipt: Some(old.receipt),
                });
            } else if let Some(Json(input)) = ctx.get::<Json<Admit>>(&attempt_key(&id)).await? {
                attempt = Some(Attempt {
                    input,
                    receipt: None,
                });
            }
            if attempt
                .as_ref()
                .is_some_and(|a| a.input.requester_id != query.requester_id)
                || (explicit && attempt.is_none())
            {
                return Err(missing());
            }
        }
        let latest_id = ctx
            .get::<Json<RequestId>>(&pointer(query.requester_id))
            .await?
            .map(|v| v.0);
        let request_id = match attempt
            .as_ref()
            .and_then(|a| a.receipt.as_ref())
            .map(|r| &r.result)
        {
            Some(AdmissionResult::Accepted { request_id, .. }) => Some(*request_id),
            _ => latest_id,
        };
        let request = if let Some(id) = request_id {
            Some(
                self.status(&ctx, id, query.requester_id, query.link_id)
                    .await?,
            )
        } else {
            None
        };
        let latest_request = if latest_id == request_id {
            request.clone()
        } else if let Some(id) = latest_id {
            Some(
                self.status(&ctx, id, query.requester_id, query.link_id)
                    .await?,
            )
        } else {
            None
        };
        let can_start_fresh = link.inactive(self.now()).is_none()
            && latest_request
                .as_ref()
                .is_none_or(|r| !matches!(r.state, RequestState::Pending | RequestState::Approved));
        if !can_start_fresh && attempt.is_none() && request.is_none() {
            return Err(missing());
        }
        Ok(Json(RequesterPage {
            link_id: link.link_id,
            repos: link.creation.repos,
            permission: link.creation.permission,
            approval_required: link.creation.approval_required,
            can_start_fresh,
            attempt,
            request,
        }))
    }
}
