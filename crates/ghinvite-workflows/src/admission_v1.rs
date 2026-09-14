//! Isolated, link-ID-keyed admission protocol (ADR 0003).
//!
//! Bind explicitly with [`bind`]; the legacy endpoint does not register this
//! service. Ingress is private: the trusted caller authenticates requesters and
//! verifies current GitHub account-admin authority before constructing commands.
//! Identity fields are assertions from that caller, never browser form authority.
//! No SQL reads/writes or installation-access policy belong in this object.

use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::{
    InvitationLinkId, InvitationLinkRepo, Permission, RepositoryIdentity, RequestId, RequestState,
    Slug,
};
use restate_sdk::context::{
    ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, InvocationHandle,
    ObjectContext, RunFuture,
};
use restate_sdk::endpoint::{Builder, ServiceOptions};
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use ghinvite_core::storage::projection::{
    AccountAdmin, AuditIntent, CreateLink, LinkSnapshot, ProjectionEnvelope, RequestSnapshot,
};

/// ULID parsing in the authoritative interface rejects aliases, overflow,
/// delimiters and missing IDs; ASCII case alone is not a different identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct AdmissionOperationId(RequestId);

impl TryFrom<String> for AdmissionOperationId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let id: RequestId = value.parse().map_err(|_| "invalid operation ID")?;
        if value.len() != 26 || value.to_ascii_uppercase() != id.to_string() {
            return Err("invalid operation ID");
        }
        Ok(Self(id))
    }
}

impl From<AdmissionOperationId> for String {
    fn from(id: AdmissionOperationId) -> Self {
        id.0.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Admit {
    pub version: u32,
    pub link_id: InvitationLinkId,
    pub operation_id: AdmissionOperationId,
    pub requester_id: u64,
    pub justification: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AdmissionReceipt {
    pub decided_at: DateTime<Utc>,
    pub result: AdmissionResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdmissionResult {
    Accepted {
        request_id: RequestId,
        state: RequestState,
        decision_deadline: Option<DateTime<Utc>>,
    },
    Rejected {
        reason: Rejection,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    Revoked,
    Expired,
    Exhausted,
    ExistingRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OperationRecord {
    input: Admit,
    receipt: AdmissionReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminLinkCommand {
    pub link_id: InvitationLinkId,
    pub admin: AccountAdmin,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestStatus {
    pub link_id: InvitationLinkId,
    pub request_id: RequestId,
    pub requester_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdmissionDecision {
    link: LinkSnapshot,
    operation: OperationRecord,
    request: Option<RequestSnapshot>,
    projection: Option<ProjectionEnvelope>,
    workflow: Option<WorkflowEnvelope>,
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

/// Internal durable projection consumer; bind via `projection_v1::bind`.
#[restate_sdk::service]
pub trait InvitationProjectionV1 {
    async fn apply_transition(input: Json<ProjectionEnvelope>) -> Result<(), TerminalError>;
}

#[restate_sdk::workflow]
pub trait InvitationRequestV1 {
    async fn run(input: Json<WorkflowEnvelope>) -> Result<(), TerminalError>;
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
    ctx.service_client::<InvitationProjectionV1Client>()
        .apply_transition(Json(envelope))
        .send()
        .invocation_id()
        .await?;
    Ok(())
}

/// Application-wide policy; every pending request snapshots its own deadline.
pub const PENDING_LIFETIME: chrono::Duration = chrono::Duration::days(7);

#[restate_sdk::object]
pub trait InvitationLinkV1 {
    async fn create(input: Json<CreateLink>) -> Result<Json<LinkSnapshot>, TerminalError>;
    async fn admit(input: Json<Admit>) -> Result<Json<AdmissionReceipt>, TerminalError>;
    async fn revoke(input: Json<AdminLinkCommand>) -> Result<Json<LinkSnapshot>, TerminalError>;
    async fn link_status(
        input: Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError>;
    async fn request_status(
        input: Json<RequestStatus>,
    ) -> Result<Json<RequestSnapshot>, TerminalError>;
}

#[derive(Default)]
pub struct InvitationLinkV1Impl {
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

impl InvitationLinkV1Impl {
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

#[cfg(feature = "integration")]
pub fn bind_with_faults(builder: Builder, faults: std::sync::Arc<Faults>) -> Builder {
    builder.bind_with_options(
        InvitationLinkV1Impl {
            faults: Some(faults),
        }
        .serve(),
        ServiceOptions::new()
            .enable_lazy_state(true)
            .idempotency_retention(std::time::Duration::from_secs(2))
            .journal_retention(std::time::Duration::from_secs(2)),
    )
}

/// Explicit opt-in for the isolated command path. Lazy state is required, not
/// an optimization: retained operation/request history must never load eagerly.
pub fn bind(builder: Builder) -> Builder {
    builder.bind_with_options(
        InvitationLinkV1Impl::default().serve(),
        ServiceOptions::new().enable_lazy_state(true),
    )
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

fn validate_key(ctx: &ObjectContext<'_>, id: InvitationLinkId) -> Result<(), TerminalError> {
    if ctx.key() != id.to_string() {
        return Err(missing());
    }
    Ok(())
}

fn validate_admin(admin: &AccountAdmin, account_id: u64) -> Result<(), TerminalError> {
    if admin.account_id != account_id || account_id == 0 || admin.user_id == 0 {
        return Err(missing());
    }
    Ok(())
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

impl InvitationLinkV1 for InvitationLinkV1Impl {
    async fn admit(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<Admit>,
    ) -> Result<Json<AdmissionReceipt>, TerminalError> {
        validate_key(&ctx, input.link_id)?;
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
            .is_some_and(|s| s.len() > 16_384)
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
        let Json(decision) = ctx
            .run(|| async {
                let now = self.now();
                let mut link = link.clone();
                let reason = if link.revoked_at.is_some() {
                    Some(Rejection::Revoked)
                } else if link.creation.expires_at.is_some_and(|at| now >= at) {
                    Some(Rejection::Expired)
                } else if link
                    .creation
                    .max_uses
                    .is_some_and(|max| link.uses >= u64::from(max))
                {
                    Some(Rejection::Exhausted)
                } else if blocking_request.as_ref().is_some_and(|r| {
                    matches!(r.state, RequestState::Pending | RequestState::Approved)
                }) {
                    Some(Rejection::ExistingRequest)
                } else {
                    None
                };
                let (result, request) = if let Some(reason) = reason {
                    (AdmissionResult::Rejected { reason }, None)
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
                    };
                    (
                        AdmissionResult::Accepted {
                            request_id,
                            state,
                            decision_deadline,
                        },
                        Some(request),
                    )
                };
                let projection = request.as_ref().map(|request| {
                    let mut events = vec![audit(
                        EventType::RequestCreated,
                        request.request_id,
                        Some(input.requester_id),
                        now,
                    )];
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
                    projection(&link, vec![request.clone()], events)
                });
                let workflow = request.as_ref().map(|request| WorkflowEnvelope {
                    version: 1,
                    request: request.clone(),
                    installation_id: link.creation.installation_id,
                    repos: link.creation.repos.clone(),
                    permission: link.creation.permission,
                    approval_required: link.creation.approval_required,
                });
                Ok::<_, HandlerError>(Json(AdmissionDecision {
                    link,
                    request,
                    projection,
                    workflow,
                    operation: OperationRecord {
                        input: input.clone(),
                        receipt: AdmissionReceipt {
                            decided_at: now,
                            result,
                        },
                    },
                }))
            })
            .name("decide_admission_v1")
            .await?;
        self.checkpoint(&ctx, "after-decision").await?;
        if let Some(request) = &decision.request {
            ctx.set("v1/link", Json(decision.link.clone()));
            self.checkpoint(&ctx, "after-link").await?;
            ctx.set(
                &format!("v1/request/{}", request.request_id),
                Json(request.clone()),
            );
            self.checkpoint(&ctx, "after-request").await?;
            ctx.set(&blocker_key, Json(request.request_id));
            self.checkpoint(&ctx, "after-blocker").await?;
        }
        ctx.set(&operation_key, Json(decision.operation.clone()));
        self.checkpoint(&ctx, "after-outcome").await?;
        if let Some(envelope) = decision.projection {
            send_projection(&ctx, envelope).await?;
        }
        self.checkpoint(&ctx, "after-projection-send").await?;
        if let Some(envelope) = decision.workflow {
            ctx.workflow_client::<InvitationRequestV1Client>(
                envelope.request.request_id.to_string(),
            )
            .run(Json(envelope))
            .send()
            .invocation_id()
            .await?;
        }
        self.checkpoint(&ctx, "after-workflow-send").await?;
        Ok(Json(decision.operation.receipt))
    }

    async fn link_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id)?;
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
        validate_key(&ctx, input.link_id)?;
        let Json(request) = ctx
            .get::<Json<RequestSnapshot>>(&format!("v1/request/{}", input.request_id))
            .await?
            .ok_or_else(missing)?;
        if input.requester_id == 0 || request.requester_id != input.requester_id {
            return Err(missing());
        }
        Ok(Json(request))
    }

    async fn revoke(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        validate_key(&ctx, input.link_id)?;
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
            .name("decide_revocation_v1")
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
        validate_key(&ctx, input.link_id)?;
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
            .name("decide_creation_v1")
            .await?;
        ctx.set("v1/link", Json(link.clone()));
        ctx.set("v1/creation", Json(link.clone()));
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
