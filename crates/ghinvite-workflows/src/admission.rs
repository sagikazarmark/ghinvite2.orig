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
pub use ghinvite_core::invitation_link::{
    AccountAdmin, CreateLink, CreateLinkError, CreateLinkOutcome, GuardrailField, LinkCreated,
    LinkSecretState, LinkSnapshot, RotateSecret, RotationRefusal, SecretRotated,
};
use ghinvite_core::link_lifecycle::{LifecycleAction, LifecycleCommand, LifecycleOutcome};
pub use ghinvite_core::request_lifecycle::{
    DecideRequest, DecisionAction, DecisionOutcome, DecisionReceipt, InitializeRequest,
    PENDING_LIFETIME, RequestSnapshot, RequestStatus, TerminalDecision,
};
pub use ghinvite_core::storage::projection::{AuditIntent, ProjectionEnvelope};

#[derive(Clone, Serialize, Deserialize)]
struct LifecycleRecord {
    input: LifecycleCommand,
    action: LifecycleAction,
    outcome: LifecycleOutcome,
}

#[derive(Clone, Serialize, Deserialize)]
struct CreationReceipt {
    input: CreateLink,
    outcome: CreateLinkOutcome,
}

fn creation_success(link: &LinkSnapshot) -> CreateLinkOutcome {
    Ok(LinkCreated {
        link_id: link.link_id,
        secret_generation: 1,
    })
}

fn creation_refused(
    ctx: &ObjectContext<'_>,
    input: CreateLink,
    refusal: CreateLinkError,
) -> Json<CreateLinkOutcome> {
    let outcome = Err(refusal);
    ctx.set(
        "creation_result",
        Json(CreationReceipt {
            input,
            outcome: outcome.clone(),
        }),
    );
    Json(outcome)
}

#[derive(Clone, Serialize, Deserialize)]
struct OperationRecord {
    input: Admit,
    receipt: AdmissionReceipt,
}

#[derive(Clone, Serialize, Deserialize)]
struct AdmissionDecision {
    #[serde(default)]
    hold_epoch: u64,
    operation: OperationRecord,
    link: Option<LinkSnapshot>,
    initialization: Option<InitializeRequest>,
    #[serde(default)]
    rerequest_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub struct InvitationLink {
    access_removal_enabled: bool,
    allow_invitation_cancellation: bool,
    organization_targets_enabled: bool,
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
/// Isolated preactivation binding. Production composition must establish the
/// cancellation executor gates before opting into this capability.
pub fn bind_with_cascade_cancellation(builder: Builder) -> Builder {
    bind_with_capabilities(builder, true, false)
}

pub fn bind_with_capabilities(
    builder: Builder,
    allow_invitation_cancellation: bool,
    organization_targets_enabled: bool,
) -> Builder {
    bind_with_all_capabilities(
        builder,
        allow_invitation_cancellation,
        organization_targets_enabled,
        false,
    )
}
pub fn bind_with_all_capabilities(
    builder: Builder,
    allow_invitation_cancellation: bool,
    organization_targets_enabled: bool,
    access_removal_enabled: bool,
) -> Builder {
    builder.bind(
        InvitationLink {
            access_removal_enabled,
            allow_invitation_cancellation,
            organization_targets_enabled,
            #[cfg(feature = "integration")]
            faults: None,
        }
        .into_service_definition()
        .options(ServiceOptions::new().enable_lazy_state(true)),
    )
}
#[cfg(feature = "integration")]
pub fn bind_with_faults(builder: Builder, faults: std::sync::Arc<Faults>) -> Builder {
    bind_faults(builder, faults, true)
}
#[cfg(feature = "integration")]
pub fn bind_with_availability_faults(builder: Builder, faults: std::sync::Arc<Faults>) -> Builder {
    bind_faults(builder, faults, false)
}
#[cfg(feature = "integration")]
fn bind_faults(
    builder: Builder,
    faults: std::sync::Arc<Faults>,
    _skip_availability: bool,
) -> Builder {
    builder.bind(
        InvitationLink {
            access_removal_enabled: false,
            allow_invitation_cancellation: false,
            organization_targets_enabled: false,
            faults: Some(faults),
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
            access_removal_enabled: false,
            allow_invitation_cancellation: false,
            organization_targets_enabled: false,
            faults: None,
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
    if !admin.acts_for(account) {
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
pub(crate) fn event(
    kind: EventType,
    id: impl ToString,
    actor: Option<u64>,
    at: DateTime<Utc>,
) -> AuditIntent {
    AuditIntent {
        event_id: format!("{kind}/{}", id.to_string()),
        kind,
        actor_id: actor,
        target_id: id.to_string(),
        effective_at: at,
        evaluated_at: at,
    }
}
pub(crate) async fn project(
    ctx: &ObjectContext<'_>,
    link: &LinkSnapshot,
    events: Vec<AuditIntent>,
) -> Result<(), TerminalError> {
    ctx.object_client::<crate::projection::InvitationProjectionClient>(ctx.key())
        .apply_transition(Json(ProjectionEnvelope {
            transition_id: format!("link/{}/{}", link.link_id, link.revision),
            link: link.clone(),
            events,
        }))
        .send()
        .await?;
    Ok(())
}

impl InvitationLink {
    fn time<'a>(
        &self,
        ctx: &'a impl crate::clock::ContextClockExt,
        name: &'static str,
    ) -> impl Future<Output = Result<DateTime<Utc>, TerminalError>> + Send + 'a {
        #[cfg(feature = "integration")]
        {
            let faults = self.faults.clone();
            crate::clock::with_clock(ctx, name, move || {
                faults
                    .as_ref()
                    .and_then(|faults| *faults.clock.lock().unwrap())
                    .unwrap_or_else(Utc::now)
            })
        }
        #[cfg(not(feature = "integration"))]
        {
            crate::clock::now(ctx, name)
        }
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
    async fn lifecycle(
        &self,
        ctx: &ObjectContext<'_>,
        input: LifecycleCommand,
        action: LifecycleAction,
    ) -> Result<Json<LifecycleOutcome>, TerminalError> {
        let current = admin_link(ctx, input.link_id, &input.admin).await?;
        let key = format!("lifecycle/{}", String::from(input.operation_id.clone()));
        if let Some(Json(old)) = ctx.get::<Json<LifecycleRecord>>(&key).await? {
            return if old.input == input && old.action == action {
                Ok(Json(old.outcome))
            } else {
                Err(conflict())
            };
        }
        let now = self.time(ctx, "lifecycle_time").await?;
        let outcome =
            ghinvite_core::link_lifecycle::decide(&current, action, input.admin.user_id, now)
                .map_err(|_| invalid())?;
        self.checkpoint(ctx, "lifecycle-before-state").await?;
        if let Ok(link) = &outcome {
            ctx.set("link", Json(link.clone()));
        }
        ctx.set(
            &key,
            Json(LifecycleRecord {
                input: input.clone(),
                action,
                outcome: outcome.clone(),
            }),
        );
        self.checkpoint(ctx, "lifecycle-after-state").await?;
        if let Ok(link) = &outcome {
            let kind = match action {
                LifecycleAction::Pause => EventType::InvitationLinkPaused,
                LifecycleAction::Resume => EventType::InvitationLinkResumed,
            };
            let mut audit = event(kind, link.link_id, Some(input.admin.user_id), now);
            audit.event_id = format!(
                "link/{}/lifecycle/{}",
                link.link_id,
                String::from(input.operation_id.clone())
            );
            project(ctx, link, vec![audit]).await?;
        }
        if let Ok(link) = &outcome
            && action == LifecycleAction::Resume
            && current.pause.as_ref().is_some_and(|pause| {
                matches!(
                    pause.reason,
                    ghinvite_core::link_lifecycle::PauseReason::AbuseSuspected { .. }
                )
            })
        {
            crate::link_control::broadcast(ctx, link, now).await?;
        }
        self.checkpoint(ctx, "lifecycle-after-projection").await?;
        Ok(Json(outcome))
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
    ) -> Result<Json<CreateLinkOutcome>, TerminalError> {
        validate_key(&ctx, input.link_id)?;
        validate_admin(&input.admin, input.account_id)?;
        let input = input.canonical_input().map_err(|_| invalid())?;
        // A business refusal is a retained decision too. A changed command
        // cannot reuse its creation identity, even after policy would allow it.
        if let Some(Json(receipt)) = ctx.get::<Json<CreationReceipt>>("creation_result").await? {
            return if receipt.input == input {
                Ok(Json(receipt.outcome))
            } else {
                Err(conflict())
            };
        }
        if let Some(Json(old)) = ctx.get::<Json<LinkSnapshot>>("creation").await? {
            return if input.clone().normalized().ok().as_ref() == Some(&old.creation) {
                Ok(Json(creation_success(&old)))
            } else {
                Err(conflict())
            };
        }
        if input.access_lifetime_seconds.is_some() && !self.access_removal_enabled {
            return Err(TerminalError::new_with_code(
                503,
                "Access expiry unavailable",
            ));
        }
        if let Err(refusal) = input
            .validate_guardrails()
            .and_then(|_| input.validate_targets())
        {
            return Ok(creation_refused(&ctx, input, refusal));
        }
        let input = input.normalized().map_err(|_| invalid())?;
        if !self.organization_targets_enabled && input.targets.organization.is_some() {
            return Err(TerminalError::new_with_code(
                400,
                "organization targets unavailable",
            ));
        }
        self.checkpoint(&ctx, "creation-before-decision").await?;
        let now = self.time(&ctx, "creation_time").await?;
        if input.expires_at.is_some_and(|expiry| expiry <= now) {
            return Ok(creation_refused(
                &ctx,
                input,
                CreateLinkError::InvalidGuardrail {
                    field: GuardrailField::ExpiresAt,
                },
            ));
        }
        let Json(link) = ctx
            .run(|| async {
                Ok::<_, HandlerError>(Json(LinkSnapshot {
                    metadata: None,
                    secret: LinkSecretState {
                        secret_generation: 1,
                        sha256: input.secret_sha256,
                    },
                    link_id: input.link_id,
                    creation: input.clone(),
                    created_at: now,
                    uses: 0,
                    revision: 1,
                    revoked_at: None,
                    revoked_by: None,
                    pause: None,
                }))
            })
            .name("decide_creation")
            .await?;
        self.checkpoint(&ctx, "creation-after-decision").await?;
        ctx.set("link", Json(link.clone()));
        self.checkpoint(&ctx, "creation-after-link").await?;
        ctx.set("creation", Json(link.clone()));
        ctx.set(
            "creation_result",
            Json(CreationReceipt {
                input: input.clone(),
                outcome: creation_success(&link),
            }),
        );
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
        Ok(Json(creation_success(&link)))
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
    async fn pause(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<LifecycleCommand>,
    ) -> Result<Json<LifecycleOutcome>, TerminalError> {
        self.lifecycle(&ctx, input, LifecycleAction::Pause).await
    }

    #[handler]
    async fn resume(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<LifecycleCommand>,
    ) -> Result<Json<LifecycleOutcome>, TerminalError> {
        self.lifecycle(&ctx, input, LifecycleAction::Resume).await
    }

    #[handler]
    async fn prepare_access_removal(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::revocation::PrepareAccessRemoval>,
    ) -> Result<Json<ghinvite_core::revocation::AccessRemovalPreview>, TerminalError> {
        let link = admin_link(&ctx, input.link_id, &input.admin).await?;
        if !self.access_removal_enabled {
            return Err(TerminalError::new_with_code(
                503,
                "access removal unavailable",
            ));
        }
        Ok(Json(
            crate::link_access_removal::prepare(&ctx, input, &link).await?,
        ))
    }

    #[handler]
    async fn revoke(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::revocation::RevokeCommand>,
    ) -> Result<Json<ghinvite_core::revocation::RevokeOutcome>, TerminalError> {
        let link = admin_link(&ctx, input.link_id, &input.admin).await?;
        if let Some(outcome) = crate::link_revocation::receipt(&ctx, &input).await? {
            return Ok(Json(outcome));
        }
        let at = self.time(&ctx, "revocation_time").await?;
        Ok(Json(
            crate::link_revocation::execute(
                &ctx,
                input,
                link,
                at,
                self.allow_invitation_cancellation,
                self.access_removal_enabled,
            )
            .await?,
        ))
    }
    #[handler]
    async fn revoke_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::revocation::RevokeCommand>,
    ) -> Result<Json<Option<ghinvite_core::revocation::RevokeOutcome>>, TerminalError> {
        admin_link(&ctx, input.link_id, &input.admin).await?;
        Ok(Json(crate::link_revocation::receipt(&ctx, &input).await?))
    }
    #[handler]
    async fn cascade_progress(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<AdminLinkCommand>,
    ) -> Result<Json<Option<ghinvite_core::revocation::CascadeProgress>>, TerminalError> {
        admin_link(&ctx, input.link_id, &input.admin).await?;
        Ok(Json(crate::link_revocation::progress(&ctx, &input).await?))
    }

    /// Operation-bound rotation stores hashes only. The retained receipt survives
    /// later rotations and revocation, exactly like an admission receipt.
    #[handler]
    async fn rotate_secret(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RotateSecret>,
    ) -> Result<Json<SecretRotated>, TerminalError> {
        let mut link = admin_link(&ctx, input.link_id, &input.admin).await?;
        let key = format!("rotation/{}", String::from(input.operation_id.clone()));
        if let Some(Json((old, receipt))) =
            ctx.get::<Json<(RotateSecret, SecretRotated)>>(&key).await?
        {
            return if old == input {
                Ok(Json(receipt))
            } else {
                Err(conflict())
            };
        }
        // A revoked link answers its own status rather than 409, which only
        // ever means this operation is bound to different input.
        let receipt = link.rotate_secret(input.new_sha256).map_err(|refusal| {
            TerminalError::new_with_code(refusal.status(), refusal.to_string())
        })?;
        let generation = receipt.secret_generation;
        let now = self.time(&ctx, "rotation_time").await?;
        ctx.set("link", Json(link.clone()));
        ctx.set(&key, Json((input.clone(), receipt.clone())));
        let mut audit = event(
            EventType::InvitationLinkSecretRotated,
            link.link_id,
            Some(input.admin.user_id),
            now,
        );
        audit.event_id = format!("link/{}/secret/{}", link.link_id, generation);
        project(&ctx, &link, vec![audit]).await?;
        Ok(Json(receipt))
    }

    #[handler]
    async fn update_metadata(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<UpdateMetadata>,
    ) -> Result<Json<LinkSnapshot>, TerminalError> {
        let mut link = admin_link(&ctx, input.link_id, &input.admin).await?;
        let metadata = ghinvite_core::invitation_link::LinkMetadata::parse(
            &input.description,
            input.internal_note.as_deref(),
        )
        .map_err(|_| invalid())?;
        if !link.update_metadata(metadata) {
            return Ok(Json(link));
        }
        let now = self.time(&ctx, "metadata_time").await?;
        let Json(mut event) = ctx
            .run(|| async {
                Ok::<_, HandlerError>(Json(event(
                    EventType::InvitationLinkMetadataUpdated,
                    link.link_id,
                    Some(input.admin.user_id),
                    now,
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
        if let Some(Json(old)) = ctx
            .get::<Json<OperationRecord>>(&operation_key(&input.operation_id))
            .await?
        {
            if !same_attempt(&old.input, &input) {
                return Err(conflict());
            }
            return Ok(Json(Attempt {
                input,
                receipt: Some(old.receipt),
            }));
        }
        let link = link(&ctx).await?;
        if !link
            .secret
            .matches(&input.secret_sha256, input.secret_generation)
        {
            return Err(missing());
        }
        let key = attempt_key(&input.operation_id);
        if let Some(Json(old)) = ctx.get::<Json<Admit>>(&key).await? {
            if !same_attempt(&old, &input) {
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
            return if same_attempt(&old.input, &input) {
                Ok(Json(old.receipt))
            } else {
                Err(conflict())
            };
        }
        let handoff_key = format!("handoff/{}", String::from(input.operation_id.clone()));
        if let Some(Json(pending)) = ctx.get::<Json<AdmissionDecision>>(&handoff_key).await? {
            if !same_attempt(&pending.operation.input, &input) {
                return Err(conflict());
            }
            let initialization = pending.initialization.ok_or_else(uncertain)?;
            let retained_link = pending.link.as_ref().ok_or_else(uncertain)?;
            crate::link_control::initialize(
                &ctx,
                retained_link,
                &initialization,
                pending.hold_epoch,
            )
            .await?;
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
            && !same_attempt(&old, &input)
        {
            return Err(conflict());
        }
        let current = ctx.get::<Json<LinkSnapshot>>("link").await?.map(|v| v.0);
        let gate_time = self.time(&ctx, "admission_gate_time").await?;
        let gate_refusal = match current.as_ref() {
            Some(link)
                if link
                    .secret
                    .matches(&input.secret_sha256, input.secret_generation) =>
            {
                link.inactive(gate_time).map(Rejection::from)
            }
            _ => Some(Rejection::NotAvailable),
        };
        if let Some(reason) = gate_refusal {
            let receipt = AdmissionReceipt {
                decided_at: gate_time,
                result: AdmissionResult::Rejected { reason },
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
        let link = current.ok_or_else(missing)?;
        ctx.set(&prepared, Json(input.clone()));
        ctx.set(
            &latest(input.requester_id),
            Json(input.operation_id.clone()),
        );
        let last_rerequest = ctx
            .get::<Json<DateTime<Utc>>>(&format!("lapsed_rerequest/{}", input.requester_id))
            .await?
            .map(|j| j.0);
        let mut rerequest = last_rerequest.is_some();
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
            if let Some(reason) =
                Rejection::open_request(verdict.state).filter(|_| verdict.blocking)
            {
                let receipt = AdmissionReceipt {
                    decided_at: verdict.evaluated_at,
                    result: AdmissionResult::Rejected { reason },
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
            rerequest |= verdict.state == RequestState::Lapsed;
        }
        self.checkpoint(&ctx, "before-decision").await?;
        self.checkpoint(&ctx, "after-availability-evidence").await?;
        let now = self.time(&ctx, "admission_time").await?;
        self.checkpoint(&ctx, "after-admission-time").await?;
        if let Some(retry_after) = last_rerequest
            .map(|at| at + chrono::Duration::days(7))
            .filter(|at| *at > now)
        {
            let receipt = AdmissionReceipt {
                decided_at: now,
                result: AdmissionResult::Rejected {
                    reason: Rejection::RerequestLimited { retry_after },
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
        let approval_required = link.creation.approval_required
            || (rerequest && link.creation.invitation_lifetime_seconds < 7 * 24 * 60 * 60);
        let hold_epoch = ctx.get::<u64>("hold_epoch").await?.unwrap_or(0);
        let mut admission_times = ctx
            .get::<Json<Vec<DateTime<Utc>>>>("admission_times")
            .await?
            .map(|v| v.0)
            .unwrap_or_default();
        // A journaled wall clock may regress across invocations. Counting a
        // future admission would create an invalid hold window; dropping it
        // would undercount. Preserve state and retry once time catches up.
        if admission_times.iter().any(|at| *at > now) {
            return Err(uncertain());
        }
        admission_times.retain(|at| *at > now - chrono::Duration::hours(1));
        let threshold = link.creation.abuse_threshold_per_hour.unwrap_or_else(|| {
            ghinvite_core::invitation_link::default_abuse_threshold(link.creation.max_uses)
        });
        let abuse_start = (admission_times.len() >= threshold as usize)
            .then(|| admission_times.iter().min().copied())
            .flatten();
        let Json(decision) = ctx
            .run(|| async {
                let reason = link
                    .inactive(now)
                    .map(Rejection::from)
                    .or_else(|| input.domain_refusal(&link.creation.email_domains, now))
                    .or_else(|| {
                        input
                            .scope_unavailable(&link.creation.targets, now)
                            .then_some(Rejection::ScopeUnavailable)
                    });
                let (result, changed, initialization) = if let Some(reason) = reason {
                    (AdmissionResult::Rejected { reason }, None, None)
                } else if let Some(window_start) = abuse_start {
                    let mut link = link.clone();
                    link.revision = link
                        .revision
                        .checked_add(1)
                        .filter(|r| *r <= ghinvite_core::storage::integer::MAX_EXACT)
                        .ok_or_else(invalid)?;
                    link.pause = Some(ghinvite_core::link_lifecycle::LinkPause {
                        epoch: link.revision,
                        at: now,
                        by: None,
                        reason: ghinvite_core::link_lifecycle::PauseReason::AbuseSuspected {
                            window_start,
                        },
                    });
                    (
                        AdmissionResult::Rejected {
                            reason: Rejection::Paused,
                        },
                        Some(link),
                        None,
                    )
                } else {
                    let mut link = link.clone();
                    let request = input
                        .accept(&mut link, RequestId::new(), approval_required, now)
                        .ok_or_else(invalid)?;
                    let (request_id, state, deadline) =
                        (request.request_id, request.state, request.decision_deadline);
                    let initialization = InitializeRequest {
                        access_lifetime_seconds: link.creation.access_lifetime_seconds,
                        budget_share_pct: link.creation.budget_share_pct,
                        invitation_lifetime_seconds: link.creation.invitation_lifetime_seconds,
                        request,
                        installation_id: link.creation.installation_id,
                        organization_installation_id: link.creation.organization_installation_id,
                        targets: link.creation.targets.clone(),
                        approval_required,
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
                    hold_epoch,
                    operation: OperationRecord {
                        input: input.clone(),
                        receipt: AdmissionReceipt {
                            decided_at: now,
                            result,
                        },
                    },
                    link: changed,
                    rerequest_at: (rerequest && initialization.is_some()).then_some(now),
                    initialization,
                }))
            })
            .name("decide_admission")
            .await?;
        self.checkpoint(&ctx, "after-decision").await?;
        if let Some(link) = &decision.link {
            ctx.set("link", Json(link.clone()));
            self.checkpoint(&ctx, "after-link").await?;
            let events = if let Some(pause) = &link.pause {
                ctx.set("hold_epoch", pause.epoch);
                let mut audit = event(
                    EventType::InvitationLinkAutoPaused,
                    link.link_id,
                    None,
                    pause.at,
                );
                audit.event_id = format!("link/{}/auto-pause/{}", link.link_id, pause.epoch);
                crate::link_control::broadcast(&ctx, link, pause.at).await?;
                vec![audit]
            } else if link
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
            if let Some(at) = decision.rerequest_at {
                ctx.set(
                    &format!("lapsed_rerequest/{}", input.requester_id),
                    Json(at),
                );
            }
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
            admission_times.push(initialization.request.admitted_at);
            ctx.set("admission_times", Json(admission_times.clone()));
            self.checkpoint(&ctx, "before-initialize").await?;
            crate::link_control::initialize(
                &ctx,
                decision.link.as_ref().ok_or_else(uncertain)?,
                &initialization,
                decision.hold_epoch,
            )
            .await?;
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
    async fn offer(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<OfferQuery>,
    ) -> Result<Json<LinkOffer>, TerminalError> {
        validate_key(&ctx, query.link_id)?;
        let current = link(&ctx).await?;
        if !current
            .secret
            .matches(&query.secret_sha256, query.secret_generation)
        {
            return Err(missing());
        }
        let now = self.time(&ctx, "offer_time").await?;
        LinkOffer::current(&current, now)
            .map(Json)
            .ok_or_else(missing)
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
        if !link
            .secret
            .matches(&query.secret_sha256, query.secret_generation)
        {
            return Err(missing());
        }
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
        let organization_facts = if link.creation.org_installation_id().is_some() {
            let owner = ghinvite_core::account::AccountInstallationRef {
                app: ghinvite_core::account::App::Org,
                account_id: link.creation.account_id,
            }
            .key()
            .map_err(|_| missing())?;
            ctx.object_client::<crate::org_eligibility::OrganizationEligibilityClient>(&owner)
                .facts()
                .call()
                .await
                .ok()
                .and_then(|facts| facts.0)
        } else {
            None
        };
        // Read time after a possible cache refresh: the observation may begin
        // later than the start of this handler, but must never be future-dated.
        let now = self.time(&ctx, "requester_page_time").await?;
        let two_factor_required = organization_facts
            .filter(|facts| {
                link.creation
                    .org_installation_id()
                    .is_some_and(|id| facts.fresh(link.creation.account_id, id, now))
            })
            .and_then(|facts| facts.two_factor_required);
        let rerequest_after = ctx
            .get::<Json<DateTime<Utc>>>(&format!("lapsed_rerequest/{}", query.requester_id))
            .await?
            .map(|j| j.0 + chrono::Duration::days(7))
            .filter(|at| *at > now);
        let can_start_fresh = rerequest_after.is_none()
            && link.inactive(now).is_none()
            && latest_request
                .as_ref()
                .is_none_or(|r| Rejection::open_request(r.state).is_none());
        let paused = link.inactive(now) == Some(ghinvite_core::Inactive::Paused);
        if !can_start_fresh && !paused && attempt.is_none() && request.is_none() {
            return Err(missing());
        }
        Ok(Json(RequesterPage {
            two_factor_required,
            email_domains: link.creation.email_domains,
            paused,
            link_id: link.link_id,
            targets: link.creation.targets,
            approval_required: link.creation.approval_required,
            can_start_fresh,
            rerequest_after,
            attempt,
            request,
        }))
    }
}

fn same_attempt(a: &Admit, b: &Admit) -> bool {
    let hashes_match = a.secret_sha256.constant_time_eq(&b.secret_sha256);
    let mut a = a.clone();
    a.secret_sha256 = b.secret_sha256;
    hashes_match & (a == *b)
}
