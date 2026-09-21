//! Link-ID-keyed admission protocol (ADR 0003).
//!
//! Bind with [`bind`]. Ingress is private: the trusted caller authenticates
//! requesters and verifies current GitHub account-admin authority before
//! constructing commands.
//! Identity fields are assertions from that caller, never browser form authority.
//! Installation observations come from the account object, after receipt replay.

mod keys;
mod rules;

use crate::availability::Eligibility;
use crate::projection::InvitationProjectionClient;
use crate::request_lifecycle::InvitationRequestClient;
use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::{
    Description, InternalNote, InvitationLinkId, InvitationLinkRepo, Permission, RepositoryScope,
    RequestId, RequestState, Slug,
};
use restate_sdk::context::{
    ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
    RunFuture,
};
use restate_sdk::endpoint::{Builder, ServiceOptions};
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use restate_sdk::service::IntoServiceDefinition;
use rules::{
    AdmissionDecision, AdmissionOutcome, AdmissionView, OperationRecord, audit, projection,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use ghinvite_core::request_lifecycle::{
    DecideRequest, DecisionAction, DecisionOutcome, DecisionReceipt, RequestStatus,
    TerminalDecision, TerminalSignal,
};
pub use ghinvite_core::storage::projection::{
    AccountAdmin, AuditIntent, CreateLink, LinkSnapshot, ProjectionEnvelope, RequestSnapshot,
};

pub use ghinvite_core::admission::{
    AdminLinkCommand, AdmissionOperationId, AdmissionReceipt, AdmissionResult, Admit, Attempt,
    AttemptQuery, Rejection, RequesterPage, UpdateMetadata,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LifecycleRecord {
    input: DecideRequest,
    receipt: DecisionReceipt,
}

/// Immutable startup input, sufficient before any projected request row exists.
/// Admin-only link metadata is deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowEnvelope {
    pub request: RequestSnapshot,
    pub installation_id: u64,
    pub repos: Vec<InvitationLinkRepo>,
    pub permission: Permission,
    pub approval_required: bool,
}

impl WorkflowEnvelope {
    pub(crate) fn from_authority(link: &LinkSnapshot, request: RequestSnapshot) -> Self {
        Self {
            request,
            installation_id: link.creation.installation_id,
            repos: link.creation.repos.clone(),
            permission: link.creation.permission,
            approval_required: link.creation.approval_required,
        }
    }
}

async fn send_projection(
    ctx: &ObjectContext<'_>,
    envelope: ProjectionEnvelope,
) -> Result<(), TerminalError> {
    ctx.object_client::<InvitationProjectionClient>(envelope.link.link_id.to_string())
        .apply_transition(Json(envelope))
        .send()
        .await?;
    Ok(())
}

/// Application-wide policy; every pending request snapshots its own deadline.
pub const PENDING_LIFETIME: chrono::Duration = chrono::Duration::days(7);

/// Private, immutable routing registry; never consults SQL or calls a link.
struct InvitationCode;

#[restate_sdk::object]
impl InvitationCode {
    #[handler]
    async fn register(
        &self,
        ctx: ObjectContext<'_>,
        Json(id): Json<InvitationLinkId>,
    ) -> Result<(), TerminalError> {
        Slug::from_string(ctx.key().into()).map_err(|_| invalid())?;
        if let Some(Json(old)) = ctx.get::<Json<InvitationLinkId>>("link").await? {
            if old != id {
                return Err(conflict());
            }
        } else {
            ctx.set("link", Json(id));
        }
        Ok(())
    }
    #[handler]
    async fn resolve(
        &self,
        ctx: ObjectContext<'_>,
        _: Json<()>,
    ) -> Result<Json<InvitationLinkId>, TerminalError> {
        Slug::from_string(ctx.key().into()).map_err(|_| missing())?;
        ctx.get("link").await?.ok_or_else(missing)
    }
}

#[derive(Default)]
pub struct InvitationLink {
    #[cfg(feature = "integration")]
    skip_availability: bool,
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<Faults>>,
}

// Feature-gated fault injection at journal boundaries. Not a command surface
// and never authoritative state; the native test drops the actual SDK task.
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

impl InvitationLink {
    async fn transition(
        &self,
        ctx: &ObjectContext<'_>,
        request: RequestSnapshot,
        command: Option<&DecideRequest>,
    ) -> Result<RequestSnapshot, TerminalError> {
        if request.state != RequestState::Pending {
            return Ok(request);
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        let blocker_key = keys::blocker(request.requester_id);
        let blocker = ctx.get::<Json<RequestId>>(&blocker_key).await?.map(|v| v.0);
        self.checkpoint(ctx, "lifecycle-before-decision").await?;
        let Json(decision) = ctx
            .run(|| async {
                let decision =
                    rules::decide_lifecycle(&link, &request, blocker, command, self.now())?;
                Ok::<_, HandlerError>(Json(decision))
            })
            .name("decide_lifecycle")
            .await?;
        self.checkpoint(ctx, "lifecycle-after-decision").await?;
        if let Some(envelope) = decision.projection {
            ctx.set(
                &keys::request(decision.request.request_id),
                Json(decision.request.clone()),
            );
            self.checkpoint(ctx, "lifecycle-after-request").await?;
            if decision.clear_blocker {
                ctx.clear(&blocker_key);
            }
            self.checkpoint(ctx, "lifecycle-after-blocker").await?;
            send_projection(ctx, envelope).await?;
            self.checkpoint(ctx, "lifecycle-after-projection-send")
                .await?;
            send_terminal(ctx, &decision.request).await?;
            self.checkpoint(ctx, "lifecycle-after-notification-send")
                .await?;
        }
        Ok(decision.request)
    }

    /// Write the decision's after-images in ADR 0003 order — link, old
    /// request, old blocker, request, blocker, outcome — then send
    /// downstream work. Each fault checkpoint marks one journal boundary.
    async fn apply_admission(
        &self,
        ctx: &ObjectContext<'_>,
        input: &Admit,
        decision: AdmissionDecision,
    ) -> Result<AdmissionReceipt, TerminalError> {
        let blocker_key = keys::blocker(input.requester_id);
        if decision.projection.is_some() {
            ctx.set(keys::LINK, Json(decision.link.clone()));
            self.checkpoint(ctx, "after-link").await?;
        }
        if let Some(expired) = &decision.expired {
            ctx.set(&keys::request(expired.request_id), Json(expired.clone()));
            self.checkpoint(ctx, "after-old-request").await?;
            ctx.clear(&blocker_key);
            self.checkpoint(ctx, "after-old-blocker").await?;
        }
        if let Some(request) = &decision.request {
            ctx.set(&keys::request(request.request_id), Json(request.clone()));
            self.checkpoint(ctx, "after-request").await?;
            ctx.set(&blocker_key, Json(request.request_id));
            self.checkpoint(ctx, "after-blocker").await?;
        }
        if let AdmissionOutcome::Decided(operation) = &decision.outcome {
            ctx.set(&keys::op(&input.operation_id), Json(operation.clone()));
        }
        ctx.set(
            &keys::latest_attempt(input.requester_id),
            Json(input.operation_id.clone()),
        );
        self.checkpoint(ctx, "after-outcome").await?;
        if let Some(envelope) = decision.projection {
            send_projection(ctx, envelope).await?;
        }
        self.checkpoint(ctx, "after-projection-send").await?;
        if let Some(expired) = &decision.expired {
            send_terminal(ctx, expired).await?;
            self.checkpoint(ctx, "after-old-notification-send").await?;
        }
        if let Some(envelope) = decision.workflow {
            ctx.workflow_client::<InvitationRequestClient>(envelope.request.request_id.to_string())
                .run(Json(envelope))
                .send()
                .await?;
        }
        self.checkpoint(ctx, "after-workflow-send").await?;
        match decision.outcome {
            AdmissionOutcome::Decided(operation) => Ok(operation.receipt),
            AdmissionOutcome::Undetermined => Err(TerminalError::new_with_code(
                503,
                "Repository availability could not be confirmed. Retry the same attempt.",
            )),
        }
    }

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
                let active = faults.stage.lock().unwrap().as_deref() == Some(stage);
                if active {
                    if faults
                        .attempts
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        == 0
                    {
                        return Err(
                            std::io::Error::other("fixture: interrupt journal boundary").into()
                        );
                    }
                    std::future::pending::<()>().await;
                }
                Ok::<_, HandlerError>(())
            })
            .name(stage)
            .retry_policy(
                restate_sdk::context::RunRetryPolicy::default()
                    .initial_delay(std::time::Duration::from_millis(100))
                    .max_delay(std::time::Duration::from_millis(200)),
            )
            .await?;
        }
        let _ = (ctx, stage);
        Ok(())
    }
}

async fn send_terminal(
    ctx: &ObjectContext<'_>,
    request: &RequestSnapshot,
) -> Result<(), TerminalError> {
    let decision = request
        .decision
        .as_ref()
        .ok_or_else(|| TerminalError::new_with_code(500, "terminal decision missing"))?;
    ctx.workflow_client::<InvitationRequestClient>(request.request_id.to_string())
        .notify(Json(TerminalSignal {
            link_id: request.link_id,
            request_id: request.request_id,
            decision_id: decision.decision_id.clone(),
            revision: request.revision,
        }))
        .send()
        .await?;
    Ok(())
}

#[cfg(feature = "integration")]
pub fn bind_with_faults(builder: Builder, faults: std::sync::Arc<Faults>) -> Builder {
    bind_objects(
        builder,
        InvitationLink {
            faults: Some(faults),
            skip_availability: true,
        },
        ServiceOptions::new()
            .enable_lazy_state(true)
            .idempotency_retention(std::time::Duration::from_secs(2))
            .journal_retention(std::time::Duration::from_secs(2)),
    )
}

/// Bind the admission objects. Lazy state is required, not
/// an optimization: retained operation/request history must never load eagerly.
pub fn bind(builder: Builder) -> Builder {
    bind_objects(
        builder,
        InvitationLink::default(),
        ServiceOptions::new().enable_lazy_state(true),
    )
}

/// Isolated protocol fixtures supply no installation integration.
#[cfg(feature = "integration")]
pub fn bind_protocol_fixture(builder: Builder) -> Builder {
    bind_objects(
        builder,
        InvitationLink {
            skip_availability: true,
            faults: None,
        },
        ServiceOptions::new().enable_lazy_state(true),
    )
}

fn bind_objects(builder: Builder, link: InvitationLink, options: ServiceOptions) -> Builder {
    builder
        .bind(InvitationCode)
        .bind(link.into_service_definition().options(options))
}

fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid command")
}

fn missing() -> TerminalError {
    TerminalError::new_with_code(404, "not found")
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "operation conflict")
}

async fn validate_key(ctx: &ObjectContext<'_>, id: InvitationLinkId) -> Result<(), TerminalError> {
    if ctx.key() != id.to_string() {
        return Err(missing());
    }
    Ok(())
}

async fn validate_signal(
    ctx: &ObjectContext<'_>,
    signal: &TerminalSignal,
) -> Result<(), TerminalError> {
    validate_key(ctx, signal.link_id).await?;
    let Json(request) = ctx
        .get::<Json<RequestSnapshot>>(&keys::request(signal.request_id))
        .await?
        .ok_or_else(missing)?;
    if request.revision != signal.revision
        || request.state == RequestState::Pending
        || request.decision.as_ref().map(|d| &d.decision_id) != Some(&signal.decision_id)
    {
        return Err(conflict());
    }
    Ok(())
}

fn validate_admin(admin: &AccountAdmin, account_id: u64) -> Result<(), TerminalError> {
    if admin.account_id != account_id || account_id == 0 || admin.user_id == 0 {
        return Err(missing());
    }
    Ok(())
}

fn normalize_decision(input: &mut DecideRequest) -> Result<String, TerminalError> {
    if let DecisionAction::Decline { reason } = &mut input.action {
        *reason = reason
            .take()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if reason.as_ref().is_some_and(|s| s.len() > 16_384) {
            return Err(invalid());
        }
    }
    Ok(keys::lifecycle_op(&input.operation_id))
}

fn normalize_creation(mut input: CreateLink) -> Result<CreateLink, TerminalError> {
    validate_admin(&input.admin, input.account_id)?;
    if input.installation_id == 0 || input.max_uses == Some(0) {
        return Err(invalid());
    }
    (input.description, input.internal_note) =
        normalize_metadata(&input.description, input.internal_note)?;
    input.repos = RepositoryScope::parse(input.repos)
        .map_err(|_| invalid())?
        .into();
    Ok(input)
}

fn normalize_metadata(
    description: &str,
    internal_note: Option<String>,
) -> Result<(String, Option<String>), TerminalError> {
    let description = Description::parse(description).map_err(|_| invalid())?;
    let internal_note = match internal_note {
        Some(note) => InternalNote::parse(&note).map_err(|_| invalid())?,
        None => None,
    };
    Ok((description.into(), internal_note.map(String::from)))
}

#[restate_sdk::object]
impl InvitationLink {
    #[handler]
    async fn update_metadata(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<UpdateMetadata>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(mut link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        validate_admin(&input.admin, link.creation.account_id)?;
        (input.description, input.internal_note) =
            normalize_metadata(&input.description, input.internal_note)?;
        if link.description() == input.description
            && link.internal_note() == input.internal_note.as_deref()
        {
            return Ok(Json(link));
        }
        link.metadata = Some(ghinvite_core::storage::projection::LinkMetadata {
            description: input.description,
            internal_note: input.internal_note,
        });
        link.revision += 1;
        let Json(event) = ctx
            .run(|| async {
                let mut event = audit(
                    EventType::InvitationLinkMetadataUpdated,
                    link.link_id,
                    Some(input.admin.user_id),
                    self.now(),
                );
                event.event_id = format!("link/{}/metadata/{}", link.link_id, link.revision);
                Ok::<_, HandlerError>(Json(event))
            })
            .name("metadata_event")
            .await?;
        ctx.set(keys::LINK, Json(link.clone()));
        send_projection(&ctx, projection(&link, vec![], vec![event])).await?;
        Ok(Json(link))
    }
    #[handler]
    async fn prepare_attempt(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<Admit>,
    ) -> Result<Json<Attempt>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        if input.normalize().is_err() || input.requester_id == 0 {
            return Err(invalid());
        }
        ctx.get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        if let Some(Json(old)) = ctx
            .get::<Json<OperationRecord>>(&keys::op(&input.operation_id))
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
        let key = keys::attempt(&input.operation_id);
        if let Some(Json(old)) = ctx.get::<Json<Admit>>(&key).await? {
            if old != input {
                return Err(conflict());
            }
        } else {
            ctx.set(&key, Json(input.clone()));
            ctx.set(
                &keys::latest_attempt(input.requester_id),
                Json(input.operation_id.clone()),
            );
        }
        Ok(Json(Attempt {
            input,
            receipt: None,
        }))
    }

    #[handler]
    async fn requester_page(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<AttemptQuery>,
    ) -> Result<Json<RequesterPage>, TerminalError> {
        validate_key(&ctx, query.link_id).await?;
        if query.requester_id == 0 {
            return Err(missing());
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        let explicit = query.operation_id.is_some();
        let operation_id = match query.operation_id {
            Some(id) => Some(id),
            None => ctx
                .get::<Json<AdmissionOperationId>>(&keys::latest_attempt(query.requester_id))
                .await?
                .map(|v| v.0),
        };
        let mut attempt = None;
        if let Some(id) = operation_id {
            if let Some(Json(old)) = ctx.get::<Json<OperationRecord>>(&keys::op(&id)).await? {
                attempt = Some(Attempt {
                    input: old.input,
                    receipt: Some(old.receipt),
                });
            } else if let Some(Json(input)) = ctx.get::<Json<Admit>>(&keys::attempt(&id)).await? {
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
        let blocker_id = ctx
            .get::<Json<RequestId>>(&keys::blocker(query.requester_id))
            .await?
            .map(|v| v.0);
        let request_id = match attempt
            .as_ref()
            .and_then(|a| a.receipt.as_ref())
            .map(|r| &r.result)
        {
            Some(AdmissionResult::Accepted { request_id, .. }) => Some(*request_id),
            _ => blocker_id,
        };
        let request = if let Some(id) = request_id {
            let Json(request) = ctx
                .get::<Json<RequestSnapshot>>(&keys::request(id))
                .await?
                .ok_or_else(missing)?;
            if request.requester_id != query.requester_id {
                return Err(missing());
            }
            let mut request = self.transition(&ctx, request, None).await?;
            if let Some(decision) = &mut request.decision {
                decision.decline_reason = None;
            }
            Some(request)
        } else {
            None
        };
        // An old receipt can name a terminal request while a newer request
        // blocks admission. Keep receipt recovery separate from fresh eligibility.
        let blocker = if blocker_id == request_id {
            request.clone()
        } else if let Some(id) = blocker_id {
            let Json(blocker) = ctx
                .get::<Json<RequestSnapshot>>(&keys::request(id))
                .await?
                .ok_or_else(missing)?;
            Some(self.transition(&ctx, blocker, None).await?)
        } else {
            None
        };
        let can_start_fresh = rules::local_rejection(&link, blocker.as_ref(), self.now()).is_none();
        if !can_start_fresh && attempt.is_none() && request.is_none() {
            return Err(missing());
        }
        Ok(Json(RequesterPage {
            link_id: link.link_id,
            invitation_code: link.invitation_code,
            repos: link.creation.repos,
            permission: link.creation.permission,
            approval_required: link.creation.approval_required,
            can_start_fresh,
            attempt,
            request,
        }))
    }
    #[handler]
    async fn delivery_progress(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<Vec<ghinvite_core::delivery::RepositoryProgress>>, TerminalError> {
        use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
        validate_key(&ctx, query.link_id).await?;
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&keys::request(query.request_id))
            .await?
            .ok_or_else(missing)?;
        if request.requester_id != query.requester_id {
            return Err(missing());
        }
        if request.state != RequestState::Approved {
            return Ok(Json(Vec::new()));
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        let plan = ctx
            .get::<Json<crate::request_lifecycle::ApprovedDispatch>>(&keys::dispatch(
                query.request_id,
            ))
            .await?;
        let mut result = Vec::new();
        for repo in &link.creation.repos {
            let stage = if let Some(Json(plan)) = &plan {
                let command = plan
                    .commands
                    .iter()
                    .find(|c| c.repo_id == repo.repo_id)
                    .ok_or_else(conflict)?;
                if ctx
                    .get::<Json<crate::request_lifecycle::SubmittedCommand>>(&keys::submitted(
                        command.invitation_id,
                    ))
                    .await?
                    .is_some()
                {
                    DispatchStage::Submitted
                } else {
                    DispatchStage::Planned
                }
            } else {
                DispatchStage::Approved
            };
            result.push(RepositoryProgress {
                repo_id: repo.repo_id,
                stage,
            });
        }
        Ok(Json(result))
    }
    #[handler]
    async fn consume_lifecycle(
        &self,
        ctx: ObjectContext<'_>,
        Json(signal): Json<TerminalSignal>,
    ) -> Result<(), TerminalError> {
        validate_signal(&ctx, &signal).await?;
        ctx.set(&keys::consumed(signal.request_id), Json(signal));
        Ok(())
    }

    #[handler]
    async fn notification_needed(
        &self,
        ctx: ObjectContext<'_>,
        Json(signal): Json<TerminalSignal>,
    ) -> Result<Json<bool>, TerminalError> {
        validate_signal(&ctx, &signal).await?;
        Ok(Json(
            ctx.get::<Json<TerminalSignal>>(&keys::consumed(signal.request_id))
                .await?
                .is_none(),
        ))
    }

    #[handler]
    async fn record_submitted(
        &self,
        ctx: ObjectContext<'_>,
        Json(submitted): Json<crate::request_lifecycle::SubmittedCommand>,
    ) -> Result<(), TerminalError> {
        let command = &submitted.command;
        validate_key(&ctx, command.link_id).await?;
        let Json(plan) = ctx
            .get::<Json<crate::request_lifecycle::ApprovedDispatch>>(&keys::dispatch(
                command.request_id,
            ))
            .await?
            .ok_or_else(missing)?;
        if !plan.commands.contains(command) || submitted.invocation_id.is_empty() {
            return Err(conflict());
        }
        let key = keys::submitted(command.invitation_id);
        if ctx
            .get::<Json<crate::request_lifecycle::SubmittedCommand>>(&key)
            .await?
            .is_none()
        {
            ctx.set(&key, Json(submitted));
        }
        Ok(())
    }

    #[handler]
    async fn delivery_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::DeliveryStatus>, TerminalError> {
        validate_key(&ctx, query.link_id).await?;
        let Json(plan) = ctx
            .get::<Json<crate::request_lifecycle::ApprovedDispatch>>(&keys::dispatch(
                query.request_id,
            ))
            .await?
            .ok_or_else(missing)?;
        if plan.input.request.requester_id != query.requester_id {
            return Err(missing());
        }
        let mut submitted = Vec::new();
        for command in &plan.commands {
            if let Some(Json(record)) = ctx.get(&keys::submitted(command.invitation_id)).await? {
                submitted.push(record);
            }
        }
        Ok(Json(crate::request_lifecycle::DeliveryStatus {
            plan,
            submitted,
        }))
    }

    #[handler]
    async fn prepare_dispatch(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::ApprovedDispatch>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&keys::request(input.request_id))
            .await?
            .ok_or_else(missing)?;
        if input.requester_id == 0 || request.requester_id != input.requester_id {
            return Err(missing());
        }
        if request.state != RequestState::Approved {
            return Err(conflict());
        }
        let key = keys::dispatch(input.request_id);
        if let Some(dispatch) = ctx.get(&key).await? {
            return Ok(dispatch);
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        let Json(dispatch) = ctx
            .run(|| async {
                let decision = request.decision.as_ref().ok_or_else(conflict)?;
                let commands = link
                    .creation
                    .repos
                    .iter()
                    .map(|repo| ghinvite_core::delivery::CreateCommand {
                        invitation_id: ghinvite_core::GithubInvitationId::new(),
                        link_id: input.link_id,
                        request_id: input.request_id,
                        approval_id: decision.decision_id.clone(),
                        account_id: link.creation.account_id,
                        installation_id: link.creation.installation_id,
                        requester_id: request.requester_id,
                        repo_id: repo.repo_id,
                        repo_full_name: repo.repo_full_name.clone(),
                        permission: link.creation.permission,
                        approved_at: decision.effective_at,
                    })
                    .collect();
                Ok::<_, HandlerError>(Json(crate::request_lifecycle::ApprovedDispatch {
                    dispatch_id: key.clone(),
                    input: WorkflowEnvelope::from_authority(&link, request.clone()),
                    commands,
                }))
            })
            .name("retain_dispatch_plan")
            .await?;
        ctx.set(&key, Json(dispatch.clone()));
        Ok(Json(dispatch))
    }

    #[handler]
    async fn admit(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<Admit>,
    ) -> Result<Json<AdmissionReceipt>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        if input.requester_id == 0 {
            return Err(missing());
        }
        input.normalize().map_err(|_| invalid())?;
        let operation_key = keys::op(&input.operation_id);
        if let Some(Json(previous)) = ctx.get::<Json<OperationRecord>>(&operation_key).await? {
            if previous.input != input {
                return Err(conflict());
            }
            return Ok(Json(previous.receipt));
        }
        // A prepared attempt with this identity must match the retried input.
        if let Some(Json(prepared)) = ctx
            .get::<Json<Admit>>(&keys::attempt(&input.operation_id))
            .await?
            && prepared != input
        {
            return Err(conflict());
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        let blocker = ctx
            .get::<Json<RequestId>>(&keys::blocker(input.requester_id))
            .await?;
        let blocking = if let Some(Json(id)) = blocker {
            Some(
                ctx.get::<Json<RequestSnapshot>>(&keys::request(id))
                    .await?
                    .ok_or_else(|| {
                        TerminalError::new_with_code(500, "missing authoritative request")
                    })?
                    .0,
            )
        } else {
            None
        };
        let view = AdmissionView { link, blocking };
        self.checkpoint(&ctx, "before-decision").await?;
        let check_availability = ctx
            .run(|| async {
                Ok::<_, HandlerError>(rules::needs_availability(&view.link, self.now()))
            })
            .name("needs_availability")
            .await?;
        #[cfg(feature = "integration")]
        let check_availability = check_availability && !self.skip_availability;
        let eligibility = if check_availability {
            let creation = &view.link.creation;
            let observation = ctx
                .object_client::<crate::availability::AccountInstallationClient>(
                    creation.account_id.to_string(),
                )
                .eligibility(Json(crate::availability::Scope {
                    account_id: creation.account_id,
                    repo_ids: creation.repos.iter().map(|repo| repo.repo_id).collect(),
                }))
                .call()
                .await;
            match observation {
                Ok(Json(observation)) => observation,
                Err(_) => Eligibility::Unknown,
            }
        } else {
            Eligibility::Available
        };
        let Json(decision) = ctx
            .run(|| async {
                let decision = rules::decide_admission(
                    &view,
                    &input,
                    &eligibility,
                    self.now(),
                    RequestId::new(),
                )?;
                Ok::<_, HandlerError>(Json(decision))
            })
            .name("decide_admission")
            .await?;
        self.checkpoint(&ctx, "after-decision").await?;
        self.apply_admission(&ctx, &input, decision).await.map(Json)
    }

    #[handler]
    async fn link_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        validate_admin(&input.admin, link.creation.account_id)?;
        Ok(Json(link))
    }

    #[handler]
    async fn request_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RequestStatus>,
    ) -> Result<Json<RequestSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&keys::request(input.request_id))
            .await?
            .ok_or_else(missing)?;
        if input.requester_id == 0 || request.requester_id != input.requester_id {
            return Err(missing());
        }
        let mut request = self.transition(&ctx, request, None).await?;
        if let Some(decision) = &mut request.decision {
            decision.decline_reason = None;
        }
        Ok(Json(request))
    }

    #[handler]
    async fn decision_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<DecideRequest>,
    ) -> Result<Json<Option<DecisionReceipt>>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        validate_admin(&input.admin, link.creation.account_id)?;
        let key = normalize_decision(&mut input)?;
        match ctx.get::<Json<LifecycleRecord>>(&key).await? {
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
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        validate_admin(&input.admin, link.creation.account_id)?;
        let key = normalize_decision(&mut input)?;
        if let Some(Json(old)) = ctx.get::<Json<LifecycleRecord>>(&key).await? {
            if old.input != input {
                return Err(conflict());
            }
            return Ok(Json(old.receipt));
        }
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&keys::request(input.request_id))
            .await?
            .ok_or_else(missing)?;
        let was_pending = request.state == RequestState::Pending;
        let request = self.transition(&ctx, request, Some(&input)).await?;
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
        let receipt = DecisionReceipt {
            request,
            outcome: if !matching {
                DecisionOutcome::Incompatible
            } else if was_pending {
                DecisionOutcome::Applied
            } else {
                DecisionOutcome::AlreadyCompleted
            },
        };
        ctx.set(
            &key,
            Json(LifecycleRecord {
                input,
                receipt: receipt.clone(),
            }),
        );
        self.checkpoint(&ctx, "lifecycle-after-outcome").await?;
        Ok(Json(receipt))
    }

    #[handler]
    async fn revoke(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>(keys::LINK)
            .await?
            .ok_or_else(missing)?;
        validate_admin(&input.admin, link.creation.account_id)?;
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
        ctx.set(keys::LINK, Json(link.clone()));
        send_projection(
            &ctx,
            projection(
                &link,
                vec![],
                vec![audit(
                    EventType::InvitationLinkRevoked,
                    link.link_id,
                    link.revoked_by,
                    link.revoked_at.unwrap(),
                )],
            ),
        )
        .await?;
        Ok(Json(link))
    }

    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<CreateLink>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let input = normalize_creation(input)?;
        if let Some(Json(link)) = ctx.get::<Json<LinkSnapshot>>(keys::LINK).await? {
            if link.creation != input {
                return Err(conflict());
            }
            // Return the original creation receipt even after later mutations.
            return ctx.get(keys::CREATION).await?.ok_or_else(missing);
        }
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
                    invitation_code: Slug::generate(&mut rand::rngs::OsRng).as_str().into(),
                    created_at: now,
                    uses: 0,
                    revision: 1,
                    revoked_at: None,
                    revoked_by: None,
                }))
            })
            .name("decide_creation")
            .await?;
        ctx.set(keys::LINK, Json(link.clone()));
        ctx.set(keys::CREATION, Json(link.clone()));
        // Complete routing before acknowledging creation; this registry never
        // calls the link, avoiding an exclusive-object cycle.
        ctx.object_client::<InvitationCodeClient>(&link.invitation_code)
            .register(Json(link.link_id))
            .call()
            .await?;
        send_projection(
            &ctx,
            projection(
                &link,
                vec![],
                vec![audit(
                    EventType::InvitationLinkCreated,
                    link.link_id,
                    Some(input.admin.user_id),
                    link.created_at,
                )],
            ),
        )
        .await?;
        Ok(Json(link))
    }
}
