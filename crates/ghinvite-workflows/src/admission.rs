//! Link-ID-keyed admission protocol (ADR 0003).
//!
//! Bind with [`bind`]. Ingress is private: the trusted caller authenticates
//! requesters and verifies current GitHub account-admin authority before
//! constructing commands.
//! Identity fields are assertions from that caller, never browser form authority.
//! Installation observations come from the account object, after receipt replay.

use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::{
    InvitationLinkId, InvitationLinkRepo, Permission, RepositoryIdentity, RequestId, RequestState,
    Slug,
};
use restate_sdk::context::{
    ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
    RunFuture,
};
use restate_sdk::endpoint::{Builder, ServiceOptions};
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OperationRecord {
    input: Admit,
    receipt: AdmissionReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LifecycleRecord {
    input: DecideRequest,
    receipt: DecisionReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LifecycleDecision {
    request: RequestSnapshot,
    projection: Option<ProjectionEnvelope>,
    clear_blocker: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdmissionDecision {
    link: LinkSnapshot,
    operation: Option<OperationRecord>,
    request: Option<RequestSnapshot>,
    projection: Option<ProjectionEnvelope>,
    workflow: Option<WorkflowEnvelope>,
    expired: Option<RequestSnapshot>,
}

/// Immutable startup input, sufficient before any projected request row exists.
/// Admin-only link metadata is deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowEnvelope {
    pub version: u32,
    pub request: RequestSnapshot,
    pub installation_id: u64,
    pub repos: Vec<InvitationLinkRepo>,
    pub permission: Permission,
    pub approval_required: bool,
}

impl WorkflowEnvelope {
    pub(crate) fn from_authority(link: &LinkSnapshot, request: RequestSnapshot) -> Self {
        Self {
            version: 1,
            request,
            installation_id: link.creation.installation_id,
            repos: link.creation.repos.clone(),
            permission: link.creation.permission,
            approval_required: link.creation.approval_required,
        }
    }
}

/// Internal durable projection consumer, keyed by link ID so each link's
/// transitions apply in send order; bind via `projection::bind`.
#[restate_sdk::object]
pub trait InvitationProjection {
    async fn apply_transition(input: Json<ProjectionEnvelope>) -> Result<(), TerminalError>;
}

#[restate_sdk::workflow]
pub trait InvitationRequest {
    async fn run(
        input: Json<WorkflowEnvelope>,
    ) -> Result<Json<crate::request_lifecycle::WorkflowResult>, TerminalError>;
    #[shared]
    async fn notify(input: Json<TerminalSignal>) -> Result<(), TerminalError>;
    #[shared]
    async fn notification_status() -> Result<Json<Option<TerminalSignal>>, TerminalError>;
}

fn audit(
    kind: EventType,
    target: impl ToString,
    actor_id: Option<u64>,
    at: DateTime<Utc>,
) -> AuditIntent {
    let target_id = target.to_string();
    AuditIntent {
        event_id: format!("v1/{kind}/{target_id}"),
        kind,
        actor_id,
        target_id,
        effective_at: at,
        evaluated_at: at,
    }
}

fn projection(
    link: &LinkSnapshot,
    requests: Vec<RequestSnapshot>,
    events: Vec<AuditIntent>,
) -> ProjectionEnvelope {
    ProjectionEnvelope {
        version: 1,
        transition_id: format!("v1/link/{}/{}", link.link_id, link.revision),
        link: link.clone(),
        requests,
        events,
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
#[restate_sdk::object]
pub trait InvitationCode {
    async fn register(input: Json<InvitationLinkId>) -> Result<(), TerminalError>;
    async fn resolve(input: Json<()>) -> Result<Json<InvitationLinkId>, TerminalError>;
}
struct InvitationCodeImpl;
impl InvitationCode for InvitationCodeImpl {
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
    async fn resolve(
        &self,
        ctx: ObjectContext<'_>,
        _: Json<()>,
    ) -> Result<Json<InvitationLinkId>, TerminalError> {
        Slug::from_string(ctx.key().into()).map_err(|_| missing())?;
        ctx.get("link").await?.ok_or_else(missing)
    }
}

#[restate_sdk::object]
pub trait InvitationLink {
    async fn update_metadata(
        input: Json<UpdateMetadata>,
    ) -> Result<Json<LinkSnapshot>, TerminalError>;
    async fn prepare_attempt(input: Json<Admit>) -> Result<Json<Attempt>, TerminalError>;
    async fn requester_page(
        input: Json<AttemptQuery>,
    ) -> Result<Json<RequesterPage>, TerminalError>;
    async fn create(input: Json<CreateLink>) -> Result<Json<LinkSnapshot>, TerminalError>;
    async fn admit(input: Json<Admit>) -> Result<Json<AdmissionReceipt>, TerminalError>;
    async fn revoke(input: Json<AdminLinkCommand>) -> Result<Json<LinkSnapshot>, TerminalError>;
    async fn link_status(
        input: Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError>;
    async fn request_status(
        input: Json<RequestStatus>,
    ) -> Result<Json<RequestSnapshot>, TerminalError>;
    async fn decide(input: Json<DecideRequest>) -> Result<Json<DecisionReceipt>, TerminalError>;
    async fn decision_status(
        input: Json<DecideRequest>,
    ) -> Result<Json<Option<DecisionReceipt>>, TerminalError>;
    async fn prepare_dispatch(
        input: Json<RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::ApprovedDispatch>, TerminalError>;
    async fn record_submitted(
        input: Json<crate::request_lifecycle::SubmittedCommand>,
    ) -> Result<(), TerminalError>;
    async fn delivery_status(
        input: Json<RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::DeliveryStatus>, TerminalError>;
    async fn delivery_progress(
        input: Json<RequestStatus>,
    ) -> Result<Json<Vec<ghinvite_core::delivery::RepositoryProgress>>, TerminalError>;
    async fn consume_lifecycle(input: Json<TerminalSignal>) -> Result<(), TerminalError>;
    async fn notification_needed(input: Json<TerminalSignal>) -> Result<Json<bool>, TerminalError>;
}

#[derive(Default)]
pub struct InvitationLinkImpl {
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

impl InvitationLinkImpl {
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
            .get::<Json<LinkSnapshot>>("v1/link")
            .await?
            .ok_or_else(missing)?;
        let blocker_key = format!("v1/blocker/{}", request.requester_id);
        let blocker = ctx.get::<Json<RequestId>>(&blocker_key).await?.map(|v| v.0);
        self.checkpoint(ctx, "lifecycle-before-decision").await?;
        let Json(decision) = ctx
            .run(|| async {
                let mut request = request.clone();
                let now = self.now();
                let event = transition_request(&mut request, command, now)?;
                let projection = event.map(|event| {
                    let mut envelope = projection(&link, vec![request.clone()], vec![event]);
                    envelope.transition_id = request.decision.as_ref().unwrap().decision_id.clone();
                    envelope
                });
                let clear_blocker = projection.is_some()
                    && request.state != RequestState::Approved
                    && blocker == Some(request.request_id);
                Ok::<_, HandlerError>(Json(LifecycleDecision {
                    request,
                    projection,
                    clear_blocker,
                }))
            })
            .name("decide_lifecycle")
            .await?;
        self.checkpoint(ctx, "lifecycle-after-decision").await?;
        if let Some(envelope) = decision.projection {
            ctx.set(
                &format!("v1/request/{}", decision.request.request_id),
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

/// All callers use the same evaluation-time arbitration, inside their complete
/// journaled decision. Expiration is effective at the snapshotted deadline.
fn transition_request(
    request: &mut RequestSnapshot,
    command: Option<&DecideRequest>,
    now: DateTime<Utc>,
) -> Result<Option<AuditIntent>, TerminalError> {
    if request.state != RequestState::Pending {
        return Ok(None);
    }
    let deadline = request
        .decision_deadline
        .ok_or_else(|| TerminalError::new_with_code(500, "pending deadline missing"))?;
    let (state, kind, actor, effective_at, reason) = if now >= deadline {
        (
            RequestState::Expired,
            EventType::RequestExpired,
            None,
            deadline,
            None,
        )
    } else if let Some(command) = command {
        match &command.action {
            DecisionAction::Approve => (
                RequestState::Approved,
                EventType::RequestApproved,
                Some(command.admin.user_id),
                now,
                None,
            ),
            DecisionAction::Decline { reason } => (
                RequestState::Declined,
                EventType::RequestDeclined,
                Some(command.admin.user_id),
                now,
                reason.clone(),
            ),
        }
    } else {
        return Ok(None);
    };
    let mut event = audit(kind, request.request_id, actor, effective_at);
    event.evaluated_at = now;
    request.state = state;
    request.revision += 1;
    request.decision = Some(TerminalDecision {
        decision_id: event.event_id.clone(),
        decided_by: actor,
        effective_at,
        evaluated_at: now,
        decline_reason: reason,
    });
    Ok(Some(event))
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
    builder.bind(InvitationCodeImpl.serve()).bind_with_options(
        InvitationLinkImpl {
            faults: Some(faults),
            skip_availability: true,
        }
        .serve(),
        ServiceOptions::new()
            .enable_lazy_state(true)
            .idempotency_retention(std::time::Duration::from_secs(2))
            .journal_retention(std::time::Duration::from_secs(2)),
    )
}

/// Bind the admission objects. Lazy state is required, not
/// an optimization: retained operation/request history must never load eagerly.
pub fn bind(builder: Builder) -> Builder {
    builder.bind(InvitationCodeImpl.serve()).bind_with_options(
        InvitationLinkImpl::default().serve(),
        ServiceOptions::new().enable_lazy_state(true),
    )
}

/// Isolated protocol fixtures supply no installation integration.
#[cfg(feature = "integration")]
pub fn bind_protocol_fixture(builder: Builder) -> Builder {
    builder.bind(InvitationCodeImpl.serve()).bind_with_options(
        InvitationLinkImpl {
            skip_availability: true,
            faults: None,
        }
        .serve(),
        ServiceOptions::new().enable_lazy_state(true),
    )
}

fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid command")
}

fn local_rejection(
    link: &LinkSnapshot,
    request: Option<&RequestSnapshot>,
    now: DateTime<Utc>,
) -> Option<Rejection> {
    if link.revoked_at.is_some() {
        Some(Rejection::Revoked)
    } else if link.creation.expires_at.is_some_and(|at| now >= at) {
        Some(Rejection::Expired)
    } else if link
        .creation
        .max_uses
        .is_some_and(|max| link.uses >= u64::from(max))
    {
        Some(Rejection::Exhausted)
    } else if request
        .is_some_and(|r| matches!(r.state, RequestState::Pending | RequestState::Approved))
    {
        Some(Rejection::ExistingRequest)
    } else {
        None
    }
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
        .get::<Json<RequestSnapshot>>(&format!("v1/request/{}", signal.request_id))
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
    Ok(format!(
        "v1/lifecycle-op/{}",
        String::from(input.operation_id.clone())
    ))
}

fn normalize_creation(mut input: CreateLink) -> Result<CreateLink, TerminalError> {
    validate_admin(&input.admin, input.account_id)?;
    input.description = input.description.trim().into();
    if input.version != 1
        || input.installation_id == 0
        || input.description.is_empty()
        || input.description.chars().count() > 120
        || input.description.contains(['\r', '\n'])
        || input.max_uses == Some(0)
        || input.repos.is_empty()
        || input.repos.len() > 100
        || input
            .internal_note
            .as_ref()
            .is_some_and(|s| s.len() > 16_384)
    {
        return Err(invalid());
    }
    input.repos.sort_by_key(|repo| repo.repo_id);
    let mut previous = None;
    for repo in &input.repos {
        if repo.repo_id == 0
            || previous == Some(repo.repo_id)
            || repo.repo_full_name.len() > 256
            || RepositoryIdentity::parse(repo.repo_full_name.clone()).is_err()
        {
            return Err(invalid());
        }
        previous = Some(repo.repo_id);
    }
    Ok(input)
}

impl InvitationLink for InvitationLinkImpl {
    async fn update_metadata(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<UpdateMetadata>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(mut link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
            .await?
            .ok_or_else(missing)?;
        validate_admin(&input.admin, link.creation.account_id)?;
        input.description = input.description.trim().into();
        input.internal_note = input
            .internal_note
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if input.description.is_empty()
            || input.description.chars().count() > 120
            || input.description.contains(['\r', '\n'])
            || input
                .internal_note
                .as_ref()
                .is_some_and(|s| s.len() > 16_384)
        {
            return Err(invalid());
        }
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
                event.event_id = format!("v1/link/{}/metadata/{}", link.link_id, link.revision);
                Ok::<_, HandlerError>(Json(event))
            })
            .name("metadata_event")
            .await?;
        ctx.set("v1/link", Json(link.clone()));
        send_projection(&ctx, projection(&link, vec![], vec![event])).await?;
        Ok(Json(link))
    }
    async fn prepare_attempt(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<Admit>,
    ) -> Result<Json<Attempt>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        input.normalize();
        if input.requester_id == 0
            || input.version != 1
            || input
                .justification
                .as_ref()
                .is_some_and(|s| s.len() > ghinvite_core::admission::MAX_JUSTIFICATION_BYTES)
        {
            return Err(invalid());
        }
        ctx.get::<Json<LinkSnapshot>>("v1/link")
            .await?
            .ok_or_else(missing)?;
        let id = String::from(input.operation_id.clone());
        if let Some(Json(old)) = ctx
            .get::<Json<OperationRecord>>(&format!("v1/op/{id}"))
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
        let key = format!("v1/attempt/{id}");
        if let Some(Json(old)) = ctx.get::<Json<Admit>>(&key).await? {
            if old != input {
                return Err(conflict());
            }
        } else {
            ctx.set(&key, Json(input.clone()));
            ctx.set(
                &format!("v1/latest-attempt/{}", input.requester_id),
                Json(input.operation_id.clone()),
            );
        }
        Ok(Json(Attempt {
            input,
            receipt: None,
        }))
    }

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
            .get::<Json<LinkSnapshot>>("v1/link")
            .await?
            .ok_or_else(missing)?;
        let explicit = query.operation_id.is_some();
        let operation_id = match query.operation_id {
            Some(id) => Some(id),
            None => ctx
                .get::<Json<AdmissionOperationId>>(&format!(
                    "v1/latest-attempt/{}",
                    query.requester_id
                ))
                .await?
                .map(|v| v.0),
        };
        let mut attempt = None;
        if let Some(id) = operation_id {
            let id = String::from(id);
            if let Some(Json(old)) = ctx
                .get::<Json<OperationRecord>>(&format!("v1/op/{id}"))
                .await?
            {
                attempt = Some(Attempt {
                    input: old.input,
                    receipt: Some(old.receipt),
                });
            } else if let Some(Json(input)) =
                ctx.get::<Json<Admit>>(&format!("v1/attempt/{id}")).await?
            {
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
            .get::<Json<RequestId>>(&format!("v1/blocker/{}", query.requester_id))
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
                .get::<Json<RequestSnapshot>>(&format!("v1/request/{id}"))
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
                .get::<Json<RequestSnapshot>>(&format!("v1/request/{id}"))
                .await?
                .ok_or_else(missing)?;
            Some(self.transition(&ctx, blocker, None).await?)
        } else {
            None
        };
        let can_start_fresh = local_rejection(&link, blocker.as_ref(), self.now()).is_none();
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
    async fn delivery_progress(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<Vec<ghinvite_core::delivery::RepositoryProgress>>, TerminalError> {
        use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
        validate_key(&ctx, query.link_id).await?;
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&format!("v1/request/{}", query.request_id))
            .await?
            .ok_or_else(missing)?;
        if request.requester_id != query.requester_id {
            return Err(missing());
        }
        if request.state != RequestState::Approved {
            return Ok(Json(Vec::new()));
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
            .await?
            .ok_or_else(missing)?;
        let plan = ctx
            .get::<Json<crate::request_lifecycle::ApprovedDispatch>>(&format!(
                "v1/dispatch/{}",
                query.request_id
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
                    .get::<Json<crate::request_lifecycle::SubmittedCommand>>(&format!(
                        "v1/submitted/{}",
                        command.invitation_id
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
    async fn consume_lifecycle(
        &self,
        ctx: ObjectContext<'_>,
        Json(signal): Json<TerminalSignal>,
    ) -> Result<(), TerminalError> {
        validate_signal(&ctx, &signal).await?;
        ctx.set(&format!("v1/consumed/{}", signal.request_id), Json(signal));
        Ok(())
    }

    async fn notification_needed(
        &self,
        ctx: ObjectContext<'_>,
        Json(signal): Json<TerminalSignal>,
    ) -> Result<Json<bool>, TerminalError> {
        validate_signal(&ctx, &signal).await?;
        Ok(Json(
            ctx.get::<Json<TerminalSignal>>(&format!("v1/consumed/{}", signal.request_id))
                .await?
                .is_none(),
        ))
    }

    async fn record_submitted(
        &self,
        ctx: ObjectContext<'_>,
        Json(submitted): Json<crate::request_lifecycle::SubmittedCommand>,
    ) -> Result<(), TerminalError> {
        let command = &submitted.command;
        validate_key(&ctx, command.link_id).await?;
        let Json(plan) = ctx
            .get::<Json<crate::request_lifecycle::ApprovedDispatch>>(&format!(
                "v1/dispatch/{}",
                command.request_id
            ))
            .await?
            .ok_or_else(missing)?;
        if !plan.commands.contains(command) || submitted.invocation_id.is_empty() {
            return Err(conflict());
        }
        let key = format!("v1/submitted/{}", command.invitation_id);
        if ctx
            .get::<Json<crate::request_lifecycle::SubmittedCommand>>(&key)
            .await?
            .is_none()
        {
            ctx.set(&key, Json(submitted));
        }
        Ok(())
    }

    async fn delivery_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::DeliveryStatus>, TerminalError> {
        validate_key(&ctx, query.link_id).await?;
        let Json(plan) = ctx
            .get::<Json<crate::request_lifecycle::ApprovedDispatch>>(&format!(
                "v1/dispatch/{}",
                query.request_id
            ))
            .await?
            .ok_or_else(missing)?;
        if plan.input.request.requester_id != query.requester_id {
            return Err(missing());
        }
        let mut submitted = Vec::new();
        for command in &plan.commands {
            if let Some(Json(record)) = ctx
                .get(&format!("v1/submitted/{}", command.invitation_id))
                .await?
            {
                submitted.push(record);
            }
        }
        Ok(Json(crate::request_lifecycle::DeliveryStatus {
            plan,
            submitted,
        }))
    }

    async fn prepare_dispatch(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::ApprovedDispatch>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&format!("v1/request/{}", input.request_id))
            .await?
            .ok_or_else(missing)?;
        if input.requester_id == 0 || request.requester_id != input.requester_id {
            return Err(missing());
        }
        if request.state != RequestState::Approved {
            return Err(conflict());
        }
        let key = format!("v1/dispatch/{}", input.request_id);
        if let Some(dispatch) = ctx.get(&key).await? {
            return Ok(dispatch);
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
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
                        version: 1,
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

    async fn admit(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<Admit>,
    ) -> Result<Json<AdmissionReceipt>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        if input.requester_id == 0 {
            return Err(missing());
        }
        input.justification = input
            .justification
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if input
            .justification
            .as_ref()
            .is_some_and(|s| s.len() > ghinvite_core::admission::MAX_JUSTIFICATION_BYTES)
        {
            return Err(invalid());
        }
        let operation_key = format!("v1/op/{}", String::from(input.operation_id.clone()));
        if let Some(Json(previous)) = ctx.get::<Json<OperationRecord>>(&operation_key).await? {
            if previous.input != input {
                return Err(conflict());
            }
            return Ok(Json(previous.receipt));
        }
        // A retained identity must reach comparison even with a future version.
        if let Some(Json(prepared)) = ctx
            .get::<Json<Admit>>(&format!(
                "v1/attempt/{}",
                String::from(input.operation_id.clone())
            ))
            .await?
            && prepared != input
        {
            return Err(conflict());
        }
        if input.version != 1 {
            return Err(invalid());
        }
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
            .await?
            .ok_or_else(missing)?;
        let blocker_key = format!("v1/blocker/{}", input.requester_id);
        let blocker = ctx.get::<Json<RequestId>>(&blocker_key).await?;
        let blocking_request = if let Some(Json(id)) = blocker {
            Some(
                ctx.get::<Json<RequestSnapshot>>(&format!("v1/request/{id}"))
                    .await?
                    .ok_or_else(|| {
                        TerminalError::new_with_code(500, "missing authoritative request")
                    })?
                    .0,
            )
        } else {
            None
        };
        self.checkpoint(&ctx, "before-decision").await?;
        let check_availability = ctx
            .run(|| async {
                Ok::<_, HandlerError>(
                    link.revoked_at.is_none()
                        && link.creation.expires_at.is_none_or(|at| self.now() < at)
                        && link
                            .creation
                            .max_uses
                            .is_none_or(|max| link.uses < u64::from(max)),
                )
            })
            .name("needs_availability")
            .await?;
        #[cfg(feature = "integration")]
        let check_availability = check_availability && !self.skip_availability;
        let (availability, availability_unknown) = if check_availability {
            let observation = ctx
                .object_client::<crate::availability::AccountInstallationClient>(
                    link.creation.account_id.to_string(),
                )
                .eligibility(Json(crate::availability::Scope {
                    account_id: link.creation.account_id,
                    repo_ids: link
                        .creation
                        .repos
                        .iter()
                        .map(|repo| repo.repo_id)
                        .collect(),
                }))
                .call()
                .await;
            let observation = match observation {
                Ok(Json(observation)) => observation,
                Err(_) => crate::availability::Eligibility::Unknown,
            };
            match observation {
                crate::availability::Eligibility::Available => (None, false),
                crate::availability::Eligibility::Unavailable { reason } => (Some(reason), false),
                crate::availability::Eligibility::Unknown => (None, true),
            }
        } else {
            (None, false)
        };
        let Json(decision) = ctx
            .run(|| async {
                let now = self.now();
                let mut link = link.clone();
                let mut blocking_request = blocking_request.clone();
                let expiry_event = if let Some(request) = &mut blocking_request {
                    transition_request(request, None, now)?
                } else {
                    None
                };
                let expired = expiry_event.as_ref().and(blocking_request.clone());
                let reason = local_rejection(&link, blocking_request.as_ref(), now)
                    .or_else(|| availability.clone());
                let (result, request) = if let Some(reason) = reason {
                    (Some(AdmissionResult::Rejected { reason }), None)
                } else if availability_unknown {
                    (None, None)
                } else {
                    let request_id = RequestId::new();
                    let state = if link.creation.approval_required {
                        RequestState::Pending
                    } else {
                        RequestState::Approved
                    };
                    let decision_deadline = link
                        .creation
                        .approval_required
                        .then_some(now + PENDING_LIFETIME);
                    link.uses = link.uses.checked_add(1).ok_or_else(|| {
                        HandlerError::from(TerminalError::new_with_code(
                            500,
                            "use counter overflow",
                        ))
                    })?;
                    link.revision += 1;
                    let request = RequestSnapshot {
                        request_id,
                        link_id: link.link_id,
                        account_id: link.creation.account_id,
                        requester_id: input.requester_id,
                        justification: input.justification.clone(),
                        state,
                        admitted_at: now,
                        decision_deadline,
                        revision: 1,
                        decision: (state == RequestState::Approved).then(|| TerminalDecision {
                            decision_id: format!("v1/request.approved/{request_id}"),
                            decided_by: None,
                            effective_at: now,
                            evaluated_at: now,
                            decline_reason: None,
                        }),
                    };
                    (
                        Some(AdmissionResult::Accepted {
                            request_id,
                            state,
                            decision_deadline,
                        }),
                        Some(request),
                    )
                };
                let mut events: Vec<_> = expiry_event.into_iter().collect();
                let mut touched: Vec<_> = expired.iter().cloned().collect();
                if let Some(request) = &request {
                    touched.push(request.clone());
                    events.push(audit(
                        EventType::RequestCreated,
                        request.request_id,
                        Some(input.requester_id),
                        now,
                    ));
                    if request.state == RequestState::Approved {
                        events.push(audit(
                            EventType::RequestApproved,
                            request.request_id,
                            None,
                            now,
                        ));
                    }
                    if link
                        .creation
                        .max_uses
                        .is_some_and(|max| link.uses == u64::from(max))
                    {
                        events.push(audit(
                            EventType::InvitationLinkExhausted,
                            link.link_id,
                            None,
                            now,
                        ));
                    }
                }
                let projection = if touched.is_empty() {
                    None
                } else {
                    // Rejection plus expiry also gets a unique link revision.
                    if request.is_none() {
                        link.revision += 1;
                    }
                    Some(projection(&link, touched, events))
                };
                let workflow = request
                    .as_ref()
                    .map(|request| WorkflowEnvelope::from_authority(&link, request.clone()));
                Ok::<_, HandlerError>(Json(AdmissionDecision {
                    link,
                    request,
                    projection,
                    workflow,
                    expired,
                    operation: result.map(|result| OperationRecord {
                        input: input.clone(),
                        receipt: AdmissionReceipt {
                            decided_at: now,
                            result,
                        },
                    }),
                }))
            })
            .name("decide_admission")
            .await?;
        self.checkpoint(&ctx, "after-decision").await?;
        if decision.projection.is_some() {
            ctx.set("v1/link", Json(decision.link.clone()));
            self.checkpoint(&ctx, "after-link").await?;
        }
        if let Some(expired) = &decision.expired {
            ctx.set(
                &format!("v1/request/{}", expired.request_id),
                Json(expired.clone()),
            );
            self.checkpoint(&ctx, "after-old-request").await?;
            ctx.clear(&blocker_key);
            self.checkpoint(&ctx, "after-old-blocker").await?;
        }
        if let Some(request) = &decision.request {
            ctx.set(
                &format!("v1/request/{}", request.request_id),
                Json(request.clone()),
            );
            self.checkpoint(&ctx, "after-request").await?;
            ctx.set(&blocker_key, Json(request.request_id));
            self.checkpoint(&ctx, "after-blocker").await?;
        }
        if let Some(operation) = &decision.operation {
            ctx.set(&operation_key, Json(operation.clone()));
        }
        ctx.set(
            &format!("v1/latest-attempt/{}", input.requester_id),
            Json(input.operation_id.clone()),
        );
        self.checkpoint(&ctx, "after-outcome").await?;
        if let Some(envelope) = decision.projection {
            send_projection(&ctx, envelope).await?;
        }
        self.checkpoint(&ctx, "after-projection-send").await?;
        if let Some(expired) = &decision.expired {
            send_terminal(&ctx, expired).await?;
            self.checkpoint(&ctx, "after-old-notification-send").await?;
        }
        if let Some(envelope) = decision.workflow {
            ctx.workflow_client::<InvitationRequestClient>(envelope.request.request_id.to_string())
                .run(Json(envelope))
                .send()
                .await?;
        }
        self.checkpoint(&ctx, "after-workflow-send").await?;
        match decision.operation {
            Some(operation) => Ok(Json(operation.receipt)),
            None => Err(TerminalError::new_with_code(
                503,
                "Repository availability could not be confirmed. Retry the same attempt.",
            )),
        }
    }

    async fn link_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
            .await?
            .ok_or_else(missing)?;
        validate_admin(&input.admin, link.creation.account_id)?;
        Ok(Json(link))
    }

    async fn request_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RequestStatus>,
    ) -> Result<Json<RequestSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&format!("v1/request/{}", input.request_id))
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

    async fn decision_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<DecideRequest>,
    ) -> Result<Json<Option<DecisionReceipt>>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
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

    async fn decide(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<DecideRequest>,
    ) -> Result<Json<DecisionReceipt>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
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
        if input.version != 1 {
            return Err(invalid());
        }
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&format!("v1/request/{}", input.request_id))
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

    async fn revoke(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let Json(link) = ctx
            .get::<Json<LinkSnapshot>>("v1/link")
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
        ctx.set("v1/link", Json(link.clone()));
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

    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<CreateLink>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id).await?;
        let input = normalize_creation(input)?;
        if let Some(Json(link)) = ctx.get::<Json<LinkSnapshot>>("v1/link").await? {
            if link.creation != input {
                return Err(conflict());
            }
            // Return the original creation receipt even after later mutations.
            return ctx.get("v1/creation").await?.ok_or_else(missing);
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
        ctx.set("v1/link", Json(link.clone()));
        ctx.set("v1/creation", Json(link.clone()));
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
