//! Persistent request authority. Calls never synchronously return to the link.
use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::request_lifecycle::{
    ApprovedDispatch, DecideRequest, DecisionOutcome, DecisionReceipt, DeliveryStatus,
    InitializeRequest, RequestSnapshot, RequestStatus, SubmittedCommand,
    SubmittedOrganizationCommand,
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
    organization_targets_enabled: bool,
    reissue_enabled: bool,
    bulk_enabled: bool,
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
    bind_with_organization_targets(builder, false)
}

pub fn bind_with_organization_targets(builder: Builder, enabled: bool) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest {
            organization_targets_enabled: enabled,
            reissue_enabled: false,
            bulk_enabled: false,
            #[cfg(feature = "integration")]
            faults: None,
        }
        .into_service_definition()
        .options(restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true)),
    )
}

/// Bind only when AccessGrant, InviteBudget and both selected delivery owners
/// are available; normal composition remains unchanged until M2 activation.
pub fn bind_with_reissue(builder: Builder, organization_targets_enabled: bool) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest {
            organization_targets_enabled,
            reissue_enabled: true,
            bulk_enabled: false,
            #[cfg(feature = "integration")]
            faults: None,
        }
        .into_service_definition()
        .options(restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true)),
    )
}

/// Composition must bind the corresponding owners before enabling either M2 path.
pub fn bind_with_features(
    builder: Builder,
    organization_targets_enabled: bool,
    reissue_enabled: bool,
    bulk_enabled: bool,
) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest {
            organization_targets_enabled,
            reissue_enabled,
            bulk_enabled,
            #[cfg(feature = "integration")]
            faults: None,
        }
        .into_service_definition()
        .options(restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true)),
    )
}

#[cfg(feature = "integration")]
pub(crate) fn bind_with_feature_faults(
    builder: Builder,
    organization_targets_enabled: bool,
    reissue_enabled: bool,
    bulk_enabled: bool,
    faults: std::sync::Arc<Faults>,
) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest {
            organization_targets_enabled,
            reissue_enabled,
            bulk_enabled,
            faults: Some(faults),
        }
        .into_service_definition()
        .options(restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true)),
    )
}

#[cfg(feature = "integration")]
pub fn bind_with_faults(builder: Builder, faults: std::sync::Arc<Faults>) -> Builder {
    bind_with_faults_and_organization_targets(builder, faults, false)
}

#[cfg(feature = "integration")]
pub fn bind_with_faults_and_organization_targets(
    builder: Builder,
    faults: std::sync::Arc<Faults>,
    organization_targets_enabled: bool,
) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest {
            faults: Some(faults),
            organization_targets_enabled,
            reissue_enabled: false,
            bulk_enabled: false,
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

#[cfg(feature = "integration")]
pub fn bind_with_reissue_faults(
    builder: Builder,
    faults: std::sync::Arc<Faults>,
    organization_targets_enabled: bool,
) -> Builder {
    use restate_sdk::service::IntoServiceDefinition;
    builder.bind(
        InvitationRequest {
            organization_targets_enabled,
            reissue_enabled: true,
            bulk_enabled: false,
            faults: Some(faults),
        }
        .into_service_definition()
        .options(restate_sdk::endpoint::ServiceOptions::new().enable_lazy_state(true)),
    )
}

impl InvitationRequest {
    pub(crate) fn validate_initial(
        &self,
        input: &InitializeRequest,
        batch: bool,
    ) -> Result<(), TerminalError> {
        let initial = &input.request;
        let valid_policy = if batch {
            !input.approval_required
                && input.budget_share_pct
                    == ghinvite_core::invitation_link::default_budget_share_pct()
                && initial.link_id.is_none()
                && initial.batch.is_some_and(|value| value.validate().is_ok())
                && initial.state == RequestState::Approved
                && initial.decision_deadline.is_none()
                && initial.domain_evidence.is_none()
                && initial.justification.is_none()
                && initial.decision.as_ref().is_some_and(|d| {
                    d.decided_by.is_some_and(|id| {
                        id > 0 && id <= ghinvite_core::storage::integer::MAX_EXACT
                    }) && d.effective_at == initial.admitted_at
                        && d.evaluated_at == initial.admitted_at
                        && d.decline_reason.is_none()
                        && d.requester_message.is_none()
                        && d.cancellation_reason.is_none()
                        && d.decision_id == format!("request.approved/{}", initial.request_id)
                })
        } else if input.approval_required {
            initial.state == RequestState::Pending
                && initial.decision.is_none()
                && initial.decision_deadline
                    == Some(
                        initial.admitted_at + ghinvite_core::request_lifecycle::PENDING_LIFETIME,
                    )
        } else {
            initial.state == RequestState::Approved
                && initial.decision_deadline.is_none()
                && initial.decision.as_ref().is_some_and(|d| {
                    d.decided_by.is_none()
                        && d.effective_at == initial.admitted_at
                        && d.evaluated_at == initial.admitted_at
                        && d.decline_reason.is_none()
                        && d.requester_message.is_none()
                        && d.cancellation_reason.is_none()
                        && d.decision_id == format!("request.approved/{}", initial.request_id)
                })
        };
        if !valid_policy
            || (!batch && (input.request.batch.is_some() || input.request.link_id.is_none()))
            || input.access_lifetime_seconds == Some(0)
            || !(3600..=604800).contains(&input.invitation_lifetime_seconds)
            || !(1..=100).contains(&input.budget_share_pct)
            || initial.revision != 1
            || initial.requester_id == 0
            || initial.account_id == 0
            || input.validate_installations().is_err()
            || input.targets.validate().is_err()
            || (!self.organization_targets_enabled && input.targets.organization.is_some())
        {
            return Err(TerminalError::new_with_code(
                400,
                "invalid admission context",
            ));
        }
        Ok(())
    }
    pub(crate) async fn initialize_inner(
        &self,
        ctx: &ObjectContext<'_>,
        input: InitializeRequest,
        batch: bool,
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
        self.validate_initial(&input, batch)?;
        crate::request_control::validate_initialization(ctx, &input).await?;
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
        self.checkpoint(ctx, "initialization-recorded").await?;
        crate::request_control::after_initialize(ctx, &input).await?;
        self.apply_pending_cascade(ctx).await?;
        self.arrange(ctx, &request(ctx).await?).await?;
        if let Some(deadline) = input.request.decision_deadline {
            let now = self.time(ctx, "schedule_expiry_time").await?;
            ctx.object_client::<InvitationRequestClient>(ctx.key())
                .tick_expire()
                .send_after((deadline - now).to_std().unwrap_or_default())
                .await?;
        }
        ctx.set("initialized", true);
        Ok(())
    }
}

impl InvitationRequest {
    pub(crate) fn time<'a>(
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

    async fn evaluated_at(&self, ctx: &ObjectContext<'_>) -> Result<DateTime<Utc>, TerminalError> {
        self.time(ctx, "evaluation_time").await
    }
    pub(crate) async fn checkpoint(
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
    crate::request_reissue::flush(ctx).await?;
    crate::request_batch_cancel::flush(ctx).await?;
    crate::request_reissue::request(ctx)
        .await?
        .ok_or_else(missing)
}

async fn expire(
    ctx: &ObjectContext<'_>,
    mut request: RequestSnapshot,
    now: DateTime<Utc>,
) -> RequestSnapshot {
    if request.expire_due(now) {
        ctx.set("request", Json(request.clone()));
    }
    request
}

pub(crate) fn events(request: &RequestSnapshot) -> Vec<AuditIntent> {
    let mut events = vec![AuditIntent {
        event_id: format!("request.created/{}", request.request_id),
        kind: EventType::RequestCreated,
        actor_id: if request.batch.is_some() {
            request.decision.as_ref().and_then(|d| d.decided_by)
        } else {
            Some(request.requester_id)
        },
        target_id: request.request_id.to_string(),
        effective_at: request.admitted_at,
        evaluated_at: request.admitted_at,
    }];
    if let Some(decision) = &request.decision {
        events.push(AuditIntent {
            event_id: decision.decision_id.clone(),
            kind: match request.state {
                RequestState::Approved | RequestState::Lapsed => EventType::RequestApproved,
                RequestState::Declined => EventType::RequestDeclined,
                RequestState::Cancelled => EventType::RequestCancelled,
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
    if request.state == RequestState::Approved
        && let Some(proof) = crate::request_reissue::proof(ctx).await?
    {
        if proof.after != *request {
            return Err(conflict());
        }
        ctx.object_client::<crate::projection::RequestProjectionClient>(ctx.key())
            .apply_reissue(Json(proof))
            .send()
            .await?;
        return Ok(());
    }

    let envelope = if request.state == RequestState::Lapsed {
        ctx.get::<Json<RequestProjectionEnvelope>>("lapse_projection")
            .await?
            .ok_or_else(conflict)?
            .0
    } else {
        let envelope = RequestProjectionEnvelope {
            request: request.clone(),
            events: events(request),
            approval: None,
        };
        if request.state == RequestState::Approved {
            ctx.set("approval_projection", Json(envelope.clone()));
        }
        envelope
    };
    if envelope.request != *request {
        return Err(conflict());
    }
    ctx.object_client::<crate::projection::RequestProjectionClient>(ctx.key())
        .apply(Json(envelope))
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
    input
        .action
        .normalize()
        .map_err(|message| TerminalError::new_with_code(400, message))?;
    Ok(format!(
        "decision/{}",
        String::from(input.operation_id.clone())
    ))
}

fn admin(request: &RequestSnapshot, input: &DecideRequest) -> Result<(), TerminalError> {
    if input.request_id != request.request_id
        || Some(input.link_id) != request.link_id
        || input.admin.account_id != request.account_id
        || input.admin.user_id == 0
    {
        return Err(missing());
    }
    Ok(())
}

impl InvitationRequest {
    /// A cascade is a local cancellation, not an administrator's decline and
    /// never a claim that an upstream GitHub invitation was deleted.
    async fn apply_pending_cascade(&self, ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
        let Some(_control) = crate::request_control::pending_cancellation(ctx).await? else {
            return Ok(());
        };
        let Some(Json(mut current)) = crate::request_reissue::request(ctx).await?.map(Json) else {
            return Ok(());
        };
        if current.state == RequestState::Pending {
            let now = self.time(ctx, "cascade_evaluation_time").await?;
            current
                .cancel_for_revoked_link(now)
                .map_err(|_| conflict())?;
            ctx.set("request", Json(current.clone()));
        }
        // Replays can repair a lost projection acknowledgement, while the
        // immutable terminal decision and audit event identities stay fixed.
        if matches!(
            current.state,
            RequestState::Cancelled | RequestState::Expired
        ) {
            project(ctx, &current).await?;
        }
        Ok(())
    }

    async fn retain_plan(
        &self,
        ctx: &ObjectContext<'_>,
        request: &RequestSnapshot,
    ) -> Result<(), TerminalError> {
        if request.state != RequestState::Approved
            || crate::request_reissue::plan(ctx).await?.map(Json).is_some()
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
                    .targets
                    .repos
                    .iter()
                    .map(|repo| ghinvite_core::delivery::CreateCommand {
                        budget_attribution: Default::default(),
                        access_lifetime_seconds: input.access_lifetime_seconds,
                        budget_share_pct: input.budget_share_pct,
                        delivery_generation: 1,
                        invitation_id: None,
                        link_id: request.link_id,
                        batch: request.batch,
                        request_id: request.request_id,
                        approval_id: decision.decision_id.clone(),
                        account_id: request.account_id,
                        installation_id: input.installation_id,
                        requester_id: request.requester_id,
                        repo_id: repo.repo_id,
                        repo_full_name: repo.repo_full_name.clone(),
                        permission: repo.grant.into(),
                        approved_at: decision.effective_at,
                    })
                    .collect();
                let organization_command = input
                    .targets
                    .organization
                    .as_ref()
                    .map(|organization| {
                        Ok::<_, TerminalError>(
                            ghinvite_core::org_delivery::OrganizationCreateCommand {
                                budget_attribution: Default::default(),
                                access_lifetime_seconds: input.access_lifetime_seconds,
                                invitation_id: None,
                                link_id: request.link_id,
                                batch: request.batch,
                                request_id: request.request_id,
                                approval_id: decision.decision_id.clone(),
                                account_id: request.account_id,
                                installation_id: input
                                    .org_installation_id()
                                    .ok_or_else(conflict)?,
                                requester_id: request.requester_id,
                                organization: organization.clone(),
                                teams: input.targets.teams.clone(),
                                delivery_generation: 1,
                                approved_at: decision.effective_at,
                                budget_share_pct: input.budget_share_pct,
                            },
                        )
                    })
                    .transpose()?;
                Ok::<_, HandlerError>(Json(ApprovedDispatch {
                    dispatch_id: format!("dispatch/{}", request.request_id),
                    input: input.clone(),
                    commands,
                    organization_command,
                }))
            })
            .name("approved_manifest")
            .await?;
        ctx.set("plan", Json(plan));
        Ok(())
    }

    async fn submit_organization(
        &self,
        ctx: &ObjectContext<'_>,
        plan: &ApprovedDispatch,
        recover: bool,
    ) -> Result<Option<SubmittedOrganizationCommand>, TerminalError> {
        let Some(command) = plan.organization_command.as_ref() else {
            return Ok(None);
        };
        if !recover
            && let Some(Json(record)) = ctx
                .get::<Json<SubmittedOrganizationCommand>>("submitted/organization")
                .await?
        {
            if record.command == *command {
                return Ok(Some(record));
            }
            if record.command.delivery_generation >= command.delivery_generation {
                return Err(conflict());
            }
        }
        crate::request_control::before_organization_create(
            ctx,
            &plan.input,
            command.organization.org_id,
            command.delivery_generation,
        )
        .await?;
        let invocation_id = ctx
            .object_client::<crate::org_delivery::OrganizationDeliveryClient>(
                command.delivery_key(),
            )
            .create(Json(command.clone()))
            .send()
            .await?
            .invocation_id()
            .to_owned();
        self.checkpoint(ctx, "after-organization-send").await?;
        let record = SubmittedOrganizationCommand {
            command: command.clone(),
            invocation_id,
        };
        ctx.set("submitted/organization", Json(record.clone()));
        Ok(Some(record))
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
    async fn cancel_batch(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::bulk_cancel::CancelRequest>,
    ) -> Result<Json<ghinvite_core::bulk_cancel::CancelRequestReceipt>, TerminalError> {
        if !self.bulk_enabled {
            return Err(TerminalError::new_with_code(503, "bulk unavailable"));
        }
        crate::request_batch_cancel::execute(self, &ctx, input)
            .await
            .map(Json)
    }

    #[handler]
    async fn reissue(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::reissue::ReissueCommand>,
    ) -> Result<Json<ghinvite_core::reissue::ReissueOutcome>, TerminalError> {
        if !self.reissue_enabled {
            return Err(TerminalError::new_with_code(503, "reissue unavailable"));
        }
        if matches!(
            input.target,
            ghinvite_core::reissue::ReissueTarget::Organization { .. }
        ) && !self.organization_targets_enabled
        {
            return Err(TerminalError::new_with_code(
                503,
                "organization reissue unavailable",
            ));
        }
        crate::request_reissue::execute(self, &ctx, input)
            .await
            .map(Json)
    }

    /// Delivery notifications carry identities only; retained owner snapshots
    /// supply final facts and must match the immutable approved manifest.
    #[handler]
    async fn on_delivery_settled(
        &self,
        ctx: ObjectContext<'_>,
        Json(notice): Json<ghinvite_core::request_completion::DeliveryNotice>,
    ) -> Result<(), TerminalError> {
        use ghinvite_core::{
            delivery_control::DeliveryKind,
            request_completion::{self as completion, Target, TargetKind, TargetResult},
        };
        use std::collections::{BTreeMap, BTreeSet};
        let mut current = request(&ctx).await?;
        if !matches!(current.state, RequestState::Approved | RequestState::Lapsed) {
            return Ok(());
        }
        let plan = crate::request_reissue::plan(&ctx)
            .await?
            .map(Json)
            .ok_or_else(conflict)?
            .0;
        let observed = match notice.kind {
            DeliveryKind::Repository => {
                let Some(command) = plan.commands.iter().find(|c| {
                    c.repo_id == notice.target_id
                        && c.delivery_generation == notice.delivery_generation
                }) else {
                    return Ok(());
                };
                let Some(snapshot) = ctx
                    .object_client::<crate::delivery::RepositoryDeliveryClient>(
                        command.delivery_key(),
                    )
                    .status()
                    .call()
                    .await?
                    .0
                else {
                    return Ok(());
                };
                if snapshot.create.command != *command {
                    return Err(conflict());
                }
                crate::delivery::validate_notice_snapshot(&command.delivery_key(), &snapshot)?;
                completion::repository(&snapshot)
                    .into_iter()
                    .collect::<Vec<_>>()
            }
            DeliveryKind::Organization => {
                let Some(command) = plan.organization_command.as_ref().filter(|c| {
                    c.organization.org_id == notice.target_id
                        && c.delivery_generation == notice.delivery_generation
                }) else {
                    return Ok(());
                };
                let Some(snapshot) = ctx
                    .object_client::<crate::org_delivery::OrganizationDeliveryClient>(
                        command.delivery_key(),
                    )
                    .status()
                    .call()
                    .await?
                    .0
                else {
                    return Ok(());
                };
                if snapshot.create.command != *command {
                    return Err(conflict());
                }
                snapshot.validate().map_err(|_| conflict())?;
                completion::organization(&snapshot)
            }
        };
        if current.state == RequestState::Lapsed {
            crate::bulk::items::notify_request(&ctx, &current, Some(notice)).await?;
            return project(&ctx, &current).await;
        }
        let mut expected = BTreeSet::new();
        for command in &plan.commands {
            expected.insert(Target {
                kind: TargetKind::Repository,
                id: command.repo_id,
                generation: command.delivery_generation,
            });
        }
        if let Some(command) = &plan.organization_command {
            expected.insert(Target {
                kind: TargetKind::Organization,
                id: command.organization.org_id,
                generation: command.delivery_generation,
            });
            for team in &command.teams {
                expected.insert(Target {
                    kind: TargetKind::Team,
                    id: team.team_id,
                    generation: command.delivery_generation,
                });
            }
        }
        let retained = ctx
            .get::<Json<Vec<TargetResult>>>("final_results")
            .await?
            .map(|j| j.0)
            .unwrap_or_default();
        let mut results: BTreeMap<_, _> = retained
            .into_iter()
            .filter(|r| expected.contains(&r.target))
            .map(|r| (r.target, r.result))
            .collect();
        for result in observed {
            if !expected.contains(&result.target)
                || results
                    .get(&result.target)
                    .is_some_and(|old| *old != result.result)
            {
                return Err(conflict());
            }
            results.insert(result.target, result.result);
        }
        let state = completion::aggregate(&expected, &results).map_err(|_| conflict())?;
        ctx.set(
            "final_results",
            Json(
                results
                    .into_iter()
                    .map(|(target, result)| TargetResult { target, result })
                    .collect::<Vec<_>>(),
            ),
        );
        ctx.set("completion", Json(state));
        if current.batch.is_some() {
            self.checkpoint(&ctx, "after-batch-final-results").await?;
        }
        crate::bulk::items::notify_request(&ctx, &current, Some(notice)).await?;
        if state != Some(completion::Completion::Lapsed) {
            return Ok(());
        }
        let now = self.time(&ctx, "request_lapsed_time").await?;
        let reissue = crate::request_reissue::proof(&ctx).await?;
        let approved = if let Some(proof) = &reissue {
            RequestProjectionEnvelope {
                request: proof.after.clone(),
                events: proof.approval_events.clone(),
                approval: None,
            }
        } else {
            ctx.get::<Json<RequestProjectionEnvelope>>("approval_projection")
                .await?
                .map(|j| j.0)
                .unwrap_or_else(|| RequestProjectionEnvelope {
                    request: plan.input.request.clone(),
                    events: events(&plan.input.request),
                    approval: None,
                })
        };
        if approved.request != current || approved.request.state != RequestState::Approved {
            return Err(conflict());
        }
        let approval_time = current.decision.as_ref().ok_or_else(conflict)?.effective_at;
        if now < approval_time {
            ctx.object_client::<InvitationRequestClient>(ctx.key())
                .on_delivery_settled(Json(notice))
                .send_after((approval_time - now).to_std().map_err(|_| conflict())?)
                .await?;
            return Ok(());
        }
        current.state = RequestState::Lapsed;
        current.revision = current
            .revision
            .checked_add(1)
            .filter(|v| *v <= ghinvite_core::storage::integer::MAX_EXACT)
            .ok_or_else(conflict)?;
        let mut audit = events(&current);
        audit.push(AuditIntent {
            event_id: if reissue.is_some() {
                format!("request.lapsed/{}/{}", current.request_id, current.revision)
            } else {
                format!("request.lapsed/{}", current.request_id)
            },
            kind: EventType::RequestLapsed,
            actor_id: None,
            target_id: current.request_id.to_string(),
            effective_at: now,
            evaluated_at: now,
        });
        let envelope = RequestProjectionEnvelope {
            request: current.clone(),
            events: audit,
            approval: Some(
                ghinvite_core::storage::projection::ApprovedRequestProjection {
                    reissue: reissue.map(Box::new),
                    request: approved.request,
                    events: approved.events,
                },
            ),
        };
        ctx.set("lapse_projection", Json(envelope));
        ctx.set("request", Json(current.clone()));
        self.checkpoint(&ctx, "after-lapsed-recorded").await?;
        project(&ctx, &current).await
    }

    #[handler]
    async fn cascade_manifest(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<ghinvite_core::revocation::RequestCascadeQuery>,
    ) -> Result<Json<ghinvite_core::revocation::RequestCascadeManifest>, TerminalError> {
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
        if ctx.key() != query.request_id.to_string() {
            return Err(missing());
        }
        Ok(Json(
            crate::request_control::cascade_manifest(&ctx, query).await?,
        ))
    }

    #[handler]
    async fn apply_link_control(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::request_control::RequestControl>,
    ) -> Result<Json<ghinvite_core::request_control::ControlReceipt>, TerminalError> {
        let receipt = crate::request_control::apply(&ctx, input).await?;
        self.apply_pending_cascade(&ctx).await?;
        Ok(Json(receipt))
    }

    #[handler]
    async fn initialize(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InitializeRequest>,
    ) -> Result<(), TerminalError> {
        self.initialize_inner(&ctx, input, false).await
    }

    /// Private bulk owner ingress; ordinary link initialization never accepts
    /// administrator-issued requests or an implicit missing source.
    #[handler]
    async fn initialize_batch(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InitializeRequest>,
    ) -> Result<(), TerminalError> {
        if !self.bulk_enabled {
            return Err(TerminalError::new_with_code(503, "bulk unavailable"));
        }
        self.initialize_inner(&ctx, input, true).await
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
            || Some(input.attempt.link_id) != request.link_id
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
            blocking: ghinvite_core::admission::Rejection::open_request(request.state).is_some(),
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
    async fn admin_dispatch(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(input): Json<ghinvite_core::request_lifecycle::AdminRequestStatus>,
    ) -> Result<Json<Option<ApprovedDispatch>>, TerminalError> {
        #[cfg(feature = "integration")]
        if self
            .faults
            .as_ref()
            .is_some_and(|f| f.unavailable.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(TerminalError::new_with_code(
                503,
                "fixture: request read unavailable",
            ));
        }
        let request = crate::request_reissue::shared_request(&ctx)
            .await?
            .ok_or_else(missing)?;
        if input.request_id != request.request_id
            || input.admin.account_id != request.account_id
            || input.admin.user_id == 0
        {
            return Err(missing());
        }
        Ok(Json(crate::request_reissue::shared_plan(&ctx).await?))
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
            || Some(input.link_id) != request.link_id
            || input.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let request = expire(&ctx, request, self.evaluated_at(&ctx).await?).await;
        project(&ctx, &request).await?;
        Ok(Json(request.requester_view()))
    }

    /// Request-ID navigation does not grant a link capability or a new admission.
    #[handler]
    async fn requester_status(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::request_lifecycle::RequesterStatus>,
    ) -> Result<Json<ghinvite_core::request_lifecycle::RequesterView>, TerminalError> {
        use ghinvite_core::{
            delivery::{DispatchStage, RepositoryProgress},
            request_lifecycle::{OrganizationProgress, RequesterView},
        };
        let request = request(&ctx).await?;
        if input.request_id != request.request_id
            || input.requester_id == 0
            || input.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let request = expire(&ctx, request, self.evaluated_at(&ctx).await?).await;
        project(&ctx, &request).await?;
        let initial = ctx
            .get::<Json<InitializeRequest>>("input")
            .await?
            .ok_or_else(missing)?
            .0;
        if initial.request.request_id != request.request_id
            || initial.request.account_id != request.account_id
            || initial.request.requester_id != request.requester_id
        {
            return Err(conflict());
        }
        let mut repositories = Vec::new();
        let mut organization = None;
        if let Some(plan) = crate::request_reissue::plan(&ctx).await? {
            for command in plan.commands {
                let submitted = ctx
                    .get::<Json<SubmittedCommand>>(&format!("submitted/{}", command.repo_id))
                    .await?
                    .is_some_and(|j| j.0.command == command);
                repositories.push(RepositoryProgress {
                    repo_id: command.repo_id,
                    delivery_generation: command.delivery_generation,
                    stage: if submitted {
                        DispatchStage::Submitted
                    } else {
                        DispatchStage::Planned
                    },
                });
            }
            if let Some(command) = plan.organization_command {
                let submitted = ctx
                    .get::<Json<SubmittedOrganizationCommand>>("submitted/organization")
                    .await?
                    .is_some_and(|j| j.0.command == command);
                organization = Some(OrganizationProgress {
                    org_id: command.organization.org_id,
                    delivery_generation: command.delivery_generation,
                    team_ids: command.teams.iter().map(|t| t.team_id).collect(),
                    stage: if submitted {
                        DispatchStage::Submitted
                    } else {
                        DispatchStage::Planned
                    },
                });
            }
        }
        Ok(Json(RequesterView::new(
            request,
            initial.targets,
            repositories,
            organization,
        )))
    }

    #[handler]
    async fn tick_expire(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        let request = expire(&ctx, request(&ctx).await?, self.evaluated_at(&ctx).await?).await;
        project(&ctx, &request).await?;
        if request.state == RequestState::Pending {
            let now = self.time(&ctx, "schedule_expiry_time").await?;
            ctx.object_client::<InvitationRequestClient>(ctx.key())
                .tick_expire()
                .send_after(
                    (request.decision_deadline.ok_or_else(conflict)? - now)
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
        let Json(authorized) = ctx
            .object_client::<crate::availability::AccountInstallationClient>(format!(
                "repo:{}",
                request.account_id
            ))
            .decision_authority(Json(input.admin.user_id))
            .call()
            .await?;
        if !authorized {
            let receipt = DecisionReceipt {
                request,
                outcome: DecisionOutcome::NotAuthorized,
            };
            ctx.set(
                &key,
                Json(DecisionRecord {
                    input,
                    receipt: receipt.clone(),
                }),
            );
            return Ok(Json(receipt));
        }
        let now = self.time(&ctx, "decision_time").await?;
        let Json(receipt) = ctx
            .run(|| async {
                let mut request = request.clone();
                if request.state == RequestState::Pending && request.decision_deadline.is_none() {
                    return Err(conflict().into());
                }
                let outcome = request.decide(input.admin.user_id, &input.action, now);
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
        crate::request_batch_cancel::flush(&ctx).await?;
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
        let Json(plan) = crate::request_reissue::plan(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)?;
        crate::bulk::items::notify_request(&ctx, &plan.input.request, None).await?;
        for command in &plan.commands {
            let key = format!("submitted/{}", command.repo_id);
            if let Some(Json(old)) = ctx.get::<Json<SubmittedCommand>>(&key).await? {
                if old.command == *command {
                    continue;
                }
                if old.command.delivery_generation >= command.delivery_generation {
                    return Err(conflict());
                }
            }
            crate::request_control::before_create(
                &ctx,
                &plan.input,
                command.repo_id,
                command.delivery_generation,
            )
            .await?;
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
        self.submit_organization(&ctx, &plan, false).await?;
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
            || Some(query.link_id) != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let Json(plan) = crate::request_reissue::plan(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)?;
        let mut submitted = Vec::new();
        for command in &plan.commands {
            crate::request_control::before_create(
                &ctx,
                &plan.input,
                command.repo_id,
                command.delivery_generation,
            )
            .await?;
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
        let submitted_organization = self.submit_organization(&ctx, &plan, true).await?;
        Ok(Json(DeliveryStatus {
            plan,
            submitted,
            submitted_organization,
        }))
    }

    #[handler]
    async fn delivery_status(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<DeliveryStatus>, TerminalError> {
        let request = crate::request_reissue::shared_request(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)?
            .0;
        if query.request_id != request.request_id
            || Some(query.link_id) != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let Json(plan) = crate::request_reissue::shared_plan(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)?;
        let mut submitted = Vec::new();
        for command in &plan.commands {
            if let Some(Json(record)) = ctx
                .get::<Json<SubmittedCommand>>(&format!("submitted/{}", command.repo_id))
                .await?
            {
                if record.command == *command {
                    submitted.push(record);
                } else if record.command.delivery_generation >= command.delivery_generation {
                    return Err(conflict());
                }
            }
        }
        let submitted_organization = if plan.organization_command.is_some() {
            ctx.get::<Json<SubmittedOrganizationCommand>>("submitted/organization")
                .await?
                .map(|j| j.0)
                .filter(|record| plan.organization_command.as_ref() == Some(&record.command))
        } else {
            None
        };
        Ok(Json(DeliveryStatus {
            plan,
            submitted,
            submitted_organization,
        }))
    }

    #[handler]
    async fn delivery_progress(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<Vec<ghinvite_core::delivery::RepositoryProgress>>, TerminalError> {
        use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
        let request = crate::request_reissue::shared_request(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)?
            .0;
        if query.request_id != request.request_id
            || Some(query.link_id) != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let Some(Json(plan)) = crate::request_reissue::shared_plan(&ctx).await?.map(Json) else {
            return Ok(Json(Vec::new()));
        };
        let mut result = Vec::new();
        for command in plan.commands {
            result.push(RepositoryProgress {
                delivery_generation: command.delivery_generation,
                repo_id: command.repo_id,
                stage: if ctx
                    .get::<Json<SubmittedCommand>>(&format!("submitted/{}", command.repo_id))
                    .await?
                    .is_some_and(|j| j.0.command == command)
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
    async fn organization_delivery_progress(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<Option<ghinvite_core::request_lifecycle::OrganizationProgress>>, TerminalError>
    {
        use ghinvite_core::{delivery::DispatchStage, request_lifecycle::OrganizationProgress};
        let request = crate::request_reissue::shared_request(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)?
            .0;
        if query.request_id != request.request_id
            || Some(query.link_id) != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        let Some(Json(plan)) = crate::request_reissue::shared_plan(&ctx).await?.map(Json) else {
            return Ok(Json(None));
        };
        let Some(command) = plan.organization_command else {
            return Ok(Json(None));
        };
        let submitted = ctx
            .get::<Json<SubmittedOrganizationCommand>>("submitted/organization")
            .await?;
        if submitted.as_ref().is_some_and(|j| {
            j.0.command != command && j.0.command.delivery_generation >= command.delivery_generation
        }) {
            return Err(conflict());
        }
        let submitted = submitted.filter(|j| j.0.command == command);
        Ok(Json(Some(OrganizationProgress {
            org_id: command.organization.org_id,
            delivery_generation: command.delivery_generation,
            team_ids: command.teams.iter().map(|t| t.team_id).collect(),
            stage: if submitted.is_some() {
                DispatchStage::Submitted
            } else {
                DispatchStage::Planned
            },
        })))
    }

    #[handler]
    async fn approved_plan(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RequestStatus>,
    ) -> Result<Json<ApprovedDispatch>, TerminalError> {
        let request = crate::request_reissue::shared_request(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)?
            .0;
        if query.request_id != request.request_id
            || Some(query.link_id) != request.link_id
            || query.requester_id != request.requester_id
        {
            return Err(missing());
        }
        crate::request_reissue::shared_plan(&ctx)
            .await?
            .map(Json)
            .ok_or_else(missing)
    }
}
