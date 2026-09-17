//! Receiving-side create receipts. Private, versioned ingress; ADR 0004.
use crate::AppState;
use ghinvite_core::{
    InvitationState,
    delivery::{CreateCommand, CreateOutcome, CreateReceipt},
    storage::Error,
};
use restate_sdk::{
    context::{
        Context, ContextClient, ContextReadState, ContextSideEffects, ContextWriteState,
        ObjectContext, RunFuture, SharedObjectContext,
    },
    endpoint::Builder,
    errors::{HandlerError, TerminalError},
    serde::Json,
};

#[restate_sdk::object]
pub trait GithubCreateV1 {
    async fn project_import() -> Result<(), TerminalError>;
    async fn import_receipt(input: Json<ImportReceipt>) -> Result<(), TerminalError>;
    async fn create(input: Json<CreateCommand>) -> Result<Json<CreateReceipt>, TerminalError>;
    async fn recheck(input: Json<CreateCommand>) -> Result<(), TerminalError>;
    #[shared]
    async fn status() -> Result<Json<Option<CreateReceipt>>, TerminalError>;
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ImportReceipt {
    pub migration_id: String,
    pub manifest_checksum: String,
    pub receipt: CreateReceipt,
}

pub struct GithubCreateV1Impl {
    state: AppState,
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<DeliveryFaults>>,
}

#[cfg(feature = "integration")]
#[derive(Default)]
pub struct DeliveryFaults {
    pub lose_http_result: std::sync::atomic::AtomicBool,
    pub lose_projection_ack: std::sync::atomic::AtomicBool,
    pub http_result_losses: std::sync::atomic::AtomicUsize,
}

pub fn bind(builder: Builder, state: AppState) -> Builder {
    builder
        .bind(
            GithubCreateV1Impl {
                state,
                #[cfg(feature = "integration")]
                faults: None,
            }
            .serve(),
        )
        .bind(DeliveryRecoveryV1Impl.serve())
}

#[cfg(feature = "integration")]
pub fn bind_with_faults(
    builder: Builder,
    state: AppState,
    faults: std::sync::Arc<DeliveryFaults>,
) -> Builder {
    builder
        .bind(
            GithubCreateV1Impl {
                state,
                faults: Some(faults),
            }
            .serve(),
        )
        .bind(DeliveryRecoveryV1Impl.serve())
}

#[restate_sdk::service]
pub trait DeliveryRecoveryV1 {
    async fn recover(
        input: Json<crate::admission_v1::RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle_v1::DeliveryStatus>, TerminalError>;
}
pub struct DeliveryRecoveryV1Impl;
impl DeliveryRecoveryV1 for DeliveryRecoveryV1Impl {
    async fn recover(
        &self,
        ctx: Context<'_>,
        Json(query): Json<crate::admission_v1::RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle_v1::DeliveryStatus>, TerminalError> {
        let link = ctx.object_client::<crate::admission_v1::InvitationLinkV1Client>(
            query.link_id.to_string(),
        );
        let Json(plan) = link.prepare_dispatch(Json(query.clone())).call().await?;
        for command in &plan.commands {
            let receiver =
                ctx.object_client::<GithubCreateV1Client>(command.invitation_id.to_string());
            let Json(receipt) = receiver.status().call().await?;
            if let Some(receipt) = receipt
                && receipt.command != *command
            {
                return Err(TerminalError::new_with_code(
                    409,
                    "receiving identity conflict",
                ));
            }
            // Confirmed replay also repairs a missing SQL projection. It
            // cannot reissue PUT; uncertain replay only reconciles.
            let invocation_id = receiver
                .create(Json(command.clone()))
                .send()
                .await?
                .invocation_id()
                .to_owned();
            link.record_submitted(Json(crate::request_lifecycle_v1::SubmittedCommand {
                command: command.clone(),
                invocation_id,
            }))
            .call()
            .await?;
        }
        link.delivery_status(Json(query)).call().await
    }
}

impl GithubCreateV1 for GithubCreateV1Impl {
    async fn project_import(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        let Json(mut receipt) = ctx
            .get::<Json<CreateReceipt>>("v1/receipt")
            .await?
            .ok_or_else(|| TerminalError::new_with_code(404, "receipt missing"))?;
        retain_recovery_time(&ctx, &mut receipt).await?;
        ctx.run(|| async { project(&self.state, &receipt).await })
            .name("project_imported_receipt")
            .await
    }
    async fn import_receipt(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ImportReceipt>,
    ) -> Result<(), TerminalError> {
        let conflict = || TerminalError::new_with_code(409, "receipt import conflict");
        if ctx.key() != input.receipt.command.invitation_id.to_string()
            || input.migration_id.is_empty()
            || input.manifest_checksum.len() != 64
            || input.receipt.revision == 0
            || input.receipt.command.version != 1
        {
            return Err(conflict());
        }
        if let Some(Json(old)) = ctx
            .get::<Json<ImportReceipt>>("migration/v1/source")
            .await?
        {
            return if old == input {
                Ok(())
            } else {
                Err(conflict())
            };
        }
        if !ctx.get_keys().await?.is_empty() {
            return Err(conflict());
        }
        // Unknown legacy effects remain fenced even if SQL is restored without
        // its invitation row. Never permit the first guarded PUT on redrive.
        if matches!(input.receipt.outcome, CreateOutcome::OutcomeUnknown) {
            ctx.run(|| async {
                self.state
                    .storage
                    .claim_delivery_attempt(&input.receipt.command)
                    .await
                    .map(|_| ())
                    .map_err(HandlerError::from)
            })
            .name("import_uncertain_http_fence")
            .await?;
        }
        ctx.set("v1/input", Json(input.receipt.command.clone()));
        ctx.set("v1/receipt", Json(input.receipt.clone()));
        ctx.set("migration/v1/source", Json(input));
        Ok(())
    }
    async fn recheck(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<CreateCommand>,
    ) -> Result<(), TerminalError> {
        let previous = ctx
            .get::<Json<CreateCommand>>("v1/input")
            .await?
            .map(|c| c.0);
        if ctx.key() != command.invitation_id.to_string() || previous.as_ref() != Some(&command) {
            return Err(TerminalError::new_with_code(
                409,
                "recheck identity conflict",
            ));
        }
        ctx.clear("v1/recheck_scheduled");
        ctx.object_client::<GithubCreateV1Client>(command.invitation_id.to_string())
            .create(Json(command))
            .send()
            .await?;
        Ok(())
    }
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<CreateReceipt>>, TerminalError> {
        Ok(Json(
            ctx.get::<Json<CreateReceipt>>("v1/receipt")
                .await?
                .map(|r| r.0),
        ))
    }

    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<CreateCommand>,
    ) -> Result<Json<CreateReceipt>, TerminalError> {
        if ctx.key() != command.invitation_id.to_string() || command.requester_id == 0 {
            return Err(TerminalError::new_with_code(400, "invalid create command"));
        }
        if let Some(Json(old)) = ctx.get::<Json<CreateCommand>>("v1/input").await?
            && old != command
        {
            return Err(TerminalError::new_with_code(409, "create input conflict"));
        }
        if command.version != 1 {
            return Err(TerminalError::new_with_code(
                400,
                "unsupported command version",
            ));
        }
        let mut previous = ctx
            .get::<Json<CreateReceipt>>("v1/receipt")
            .await?
            .map(|r| r.0);
        if let Some(receipt) = &mut previous
            && receipt.outcome.confirmed()
        {
            retain_recovery_time(&ctx, receipt).await?;
            ctx.run(|| async { project(&self.state, receipt).await })
                .name("repair_create_projection")
                .await?;
            return Ok(Json(receipt.clone()));
        }
        let Json(plan) = ctx
            .object_client::<crate::admission_v1::InvitationLinkV1Client>(
                command.link_id.to_string(),
            )
            .prepare_dispatch(Json(crate::admission_v1::RequestStatus {
                link_id: command.link_id,
                request_id: command.request_id,
                requester_id: command.requester_id,
            }))
            .call()
            .await?;
        if !plan.commands.contains(&command) {
            return Err(TerminalError::new_with_code(
                409,
                "command differs from approved plan",
            ));
        }
        ctx.set("v1/input", Json(command.clone()));
        let Json(mut receipt) = ctx
            .run(|| async {
                let receipt = attempt(&self.state, &command, previous.is_none()).await?;
                #[cfg(feature = "integration")]
                if let Some(faults) = &self.faults {
                    use std::sync::atomic::Ordering;
                    if receipt.outcome.confirmed()
                        && faults.lose_http_result.swap(false, Ordering::SeqCst)
                    {
                        faults.http_result_losses.fetch_add(1, Ordering::SeqCst);
                        return Err(std::io::Error::other(
                            "fixture: GitHub result lost before journal acknowledgement",
                        )
                        .into());
                    }
                }
                Ok::<_, HandlerError>(Json(receipt))
            })
            .name("guarded_github_create")
            .await?;
        receipt.revision = previous.as_ref().map_or(1, |r| r.revision + 1);
        if matches!(
            previous.as_ref().map(|r| &r.outcome),
            Some(CreateOutcome::OutcomeUnknown)
        ) && matches!(receipt.outcome, CreateOutcome::Blocked { .. })
        {
            receipt.outcome = CreateOutcome::OutcomeUnknown;
        }
        ctx.set("v1/receipt", Json(receipt.clone()));
        ctx.run(|| async {
            project(&self.state, &receipt).await?;
            #[cfg(feature = "integration")]
            if let Some(faults) = &self.faults {
                if faults
                    .lose_projection_ack
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(std::io::Error::other(
                        "fixture: SQL committed but acknowledgement lost",
                    )
                    .into());
                }
            }
            Ok::<_, HandlerError>(())
        })
        .name("project_create_receipt")
        .await?;
        if matches!(receipt.outcome, CreateOutcome::Blocked { .. })
            && ctx.get::<bool>("v1/recheck_scheduled").await?.is_none()
        {
            ctx.set("v1/recheck_scheduled", true);
            ctx.object_client::<GithubCreateV1Client>(command.invitation_id.to_string())
                .recheck(Json(command))
                .send_after(std::time::Duration::from_secs(3600))
                .await?;
        }
        Ok(Json(receipt))
    }
}

async fn attempt(
    state: &AppState,
    command: &CreateCommand,
    has_no_retained_receipt: bool,
) -> Result<CreateReceipt, HandlerError> {
    let dependency = || Error::ProjectionDependency;
    let request = state
        .storage
        .get_invitation_request(command.request_id)
        .await?
        .ok_or_else(dependency)?;
    let link = state
        .storage
        .get_invitation_link_by_id(command.link_id)
        .await?
        .ok_or_else(dependency)?;
    state
        .storage
        .get_user(command.requester_id)
        .await?
        .ok_or_else(dependency)?;
    if request.invitation_link_id != command.link_id
        || request.requester_id != command.requester_id
        || link.account_id != command.account_id
        || link.permission != command.permission
        || !link
            .repos
            .iter()
            .any(|r| r.repo_id == command.repo_id && r.repo_full_name == command.repo_full_name)
    {
        return Err(TerminalError::new_with_code(409, "projected create identity conflict").into());
    }
    if request.state != ghinvite_core::RequestState::Approved {
        return Err(Error::ProjectionDependency.into());
    }
    if let Some(existing) = state
        .storage
        .get_github_invitation(command.invitation_id)
        .await?
    {
        if existing.invitation_request_id != command.request_id
            || existing.repo_id != command.repo_id
        {
            return Err(TerminalError::new_with_code(409, "imported invitation conflict").into());
        }
        // Historical Sent + upstream ID is affirmative create evidence. Other
        // lifecycle states alone cannot reconstruct an original create outcome.
        if let Some(upstream_id) = existing.github_invitation_id {
            return Ok(CreateReceipt {
                command: command.clone(),
                outcome: CreateOutcome::Created { upstream_id },
                revision: 1,
                confirmed_at: Some(chrono::Utc::now()),
                recovered: true,
            });
        }
        // Fence ambiguous legacy Sending/terminal rows before reconciliation.
        if has_no_retained_receipt {
            state.storage.claim_delivery_attempt(command).await?;
        }
    }
    let attempted = state
        .storage
        .delivery_attempt_exists(command.invitation_id)
        .await?;
    let blocked = |reason: &str| CreateReceipt {
        command: command.clone(),
        outcome: if attempted {
            CreateOutcome::OutcomeUnknown
        } else {
            CreateOutcome::Blocked {
                reason: reason.into(),
            }
        },
        revision: 1,
        confirmed_at: None,
        recovered: false,
    };
    let Some(account) = state
        .storage
        .get_active_installation_by_account_id(command.account_id)
        .await?
    else {
        return Ok(blocked("installation unavailable"));
    };
    let repo = ghinvite_core::RepositoryIdentity::parse(command.repo_full_name.clone())
        .map_err(|_| TerminalError::new_with_code(400, "invalid repository"))?;
    if let ghinvite_core::SelectedRepos::Subset(ids) = &account.selected_repos
        && !ids.contains(&command.repo_id)
    {
        return Ok(blocked("repository unavailable"));
    }
    match state
        .github
        .get_repo(account.installation_id, repo.owner(), repo.name())
        .await
    {
        Ok(r) if r.id == command.repo_id => (),
        _ => return Ok(blocked("repository unavailable or identity unverified")),
    };
    let user = match state
        .github
        .verified_user(account.installation_id, command.requester_id)
        .await
    {
        Ok(user) => user,
        Err(_) => return Ok(blocked("requester identity unverified")),
    };
    // A database write fence protects retries of this run closure even when
    // GitHub succeeded but Restate never journaled its response. It never expires.
    let generation = state.storage.claim_delivery_attempt(command).await?;
    let outcome = if let Some(generation) = generation {
        match state
            .github
            .add_collaborator(
                account.installation_id,
                repo.owner(),
                repo.name(),
                &user.login,
                command.permission,
            )
            .await
        {
            Ok(Some(upstream_id)) => CreateOutcome::Created { upstream_id },
            Ok(None) => CreateOutcome::AlreadyCollaborator,
            Err(ghinvite_github::Error::Status {
                status: 401 | 403 | 404 | 429,
                ..
            }) => {
                state
                    .storage
                    .reject_delivery_attempt(command.invitation_id, generation)
                    .await?;
                CreateOutcome::Blocked {
                    reason: "GitHub rejected access or rate limited delivery".into(),
                }
            }
            Err(ghinvite_github::Error::Status {
                status: status @ (400 | 409 | 422),
                ..
            }) => CreateOutcome::Failed { status },
            Err(_) => CreateOutcome::OutcomeUnknown,
        }
    } else {
        // Reconciliation is read-only. Absence, API failure, or a subsequently
        // declined invitation never permits another write of this operation.
        let pending = state
            .github
            .list_invitations(account.installation_id, repo.owner(), repo.name())
            .await;
        match pending {
            Ok(items) => match items.iter().find(|i| {
                i.invitee.id == command.requester_id
                    && permission_matches(&i.permissions, command.permission)
            }) {
                Some(item) => CreateOutcome::Created {
                    upstream_id: item.id,
                },
                None => match state
                    .github
                    .collaborator_permission(
                        account.installation_id,
                        repo.owner(),
                        repo.name(),
                        &user.login,
                    )
                    .await
                {
                    Ok((id, permission))
                        if id == command.requester_id
                            && permission_matches(&permission, command.permission) =>
                    {
                        CreateOutcome::AlreadyCollaborator
                    }
                    _ => CreateOutcome::OutcomeUnknown,
                },
            },
            Err(_) => CreateOutcome::OutcomeUnknown,
        }
    };
    Ok(CreateReceipt {
        command: command.clone(),
        confirmed_at: outcome.confirmed().then(chrono::Utc::now),
        recovered: false,
        outcome,
        revision: 1,
    })
}

fn permission_matches(value: &str, permission: ghinvite_core::Permission) -> bool {
    value == permission.to_string()
        || value
            == match permission {
                ghinvite_core::Permission::Pull => "read",
                ghinvite_core::Permission::Push => "write",
                ghinvite_core::Permission::Triage => "triage",
                ghinvite_core::Permission::Maintain => "maintain",
                ghinvite_core::Permission::Admin => "admin",
            }
}

async fn retain_recovery_time(
    ctx: &ObjectContext<'_>,
    receipt: &mut CreateReceipt,
) -> Result<(), TerminalError> {
    if receipt.outcome.confirmed() && receipt.confirmed_at.is_none() {
        let Json(at) = ctx
            .run(|| async { Ok::<_, HandlerError>(Json(chrono::Utc::now())) })
            .name("observe_retained_create_outcome")
            .await?;
        receipt.confirmed_at = Some(at);
        receipt.recovered = true;
        receipt.revision += 1;
        ctx.set("v1/receipt", Json(receipt.clone()));
    }
    Ok(())
}

async fn project(state: &AppState, receipt: &CreateReceipt) -> Result<(), HandlerError> {
    let command = &receipt.command;
    if let Some(existing) = state
        .storage
        .get_github_invitation(command.invitation_id)
        .await?
    {
        if existing.invitation_request_id != command.request_id
            || existing.repo_id != command.repo_id
        {
            return Err(Error::ProjectionInvariant("invitation identity conflict".into()).into());
        }
    } else {
        state
            .storage
            .insert_github_invitation(&ghinvite_core::GithubInvitation {
                id: command.invitation_id,
                invitation_request_id: command.request_id,
                repo_id: command.repo_id,
                github_invitation_id: None,
                state: InvitationState::Sending,
                error_message: None,
                created_at: command.approved_at,
                updated_at: command.approved_at,
            })
            .await?;
    }
    state.storage.project_delivery(receipt).await?;
    Ok(())
}
