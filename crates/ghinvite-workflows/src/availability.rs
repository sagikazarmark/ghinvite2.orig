//! Account-keyed installation convergence. Never owns link or request history.
use crate::{
    AppState,
    installation::{OnboardInput, UninstallInput},
};
use ghinvite_core::{Account, SelectedRepos, admission::Rejection};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Scope {
    pub account_id: u64,
    pub repo_ids: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Eligibility {
    Available,
    Unavailable { reason: Rejection },
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observation {
    Available { repo_ids: Vec<u64> },
    Unavailable,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InstallationStatus {
    pub account: Option<Account>,
    pub observation: Observation,
}

#[restate_sdk::object]
pub trait AccountInstallationV1 {
    async fn retry_uninstall(input: Json<UninstallInput>) -> Result<(), TerminalError>;
    async fn recheck() -> Result<(), TerminalError>;
    async fn onboard(input: Json<OnboardInput>) -> Result<(), TerminalError>;
    async fn refresh(input: Json<u64>) -> Result<(), TerminalError>;
    async fn uninstall(input: Json<UninstallInput>) -> Result<(), TerminalError>;
    async fn status() -> Result<Json<InstallationStatus>, TerminalError>;
    async fn eligibility(input: Json<Scope>) -> Result<Json<Eligibility>, TerminalError>;
}
pub struct AccountInstallationV1Impl {
    pub state: AppState,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallationProjection {
    Onboard {
        input: OnboardInput,
    },
    Repos {
        input: crate::installation::ReposChangedInput,
    },
    Uninstall {
        input: UninstallInput,
    },
}
#[restate_sdk::object]
pub trait InstallationProjectionV1 {
    async fn apply(input: Json<InstallationProjection>) -> Result<(), TerminalError>;
}
pub struct InstallationProjectionV1Impl {
    pub state: AppState,
}
impl InstallationProjectionV1 for InstallationProjectionV1Impl {
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InstallationProjection>,
    ) -> Result<(), TerminalError> {
        use ghinvite_core::audit::{ActorKind, AuditEvent, EventType, TargetKind};
        let account_id: u64 = ctx.key().parse().map_err(|_| invalid())?;
        let Json(event) = ctx.run(|| async {
            let (id, event_type, actor_id, metadata) = match &input {
                InstallationProjection::Onboard { input } => (input.installation_id, EventType::InstallationCreated, Some(input.actor_user_id), serde_json::json!({"account_login":input.account_login,"account_type":input.account_type.to_string()})),
                InstallationProjection::Repos { input } => (input.installation_id, EventType::InstallationReposChanged, None, serde_json::json!({"selected_repos_kind":"subset"})),
                InstallationProjection::Uninstall { input } => (input.installation_id, EventType::InstallationUninstalled, None, serde_json::json!({"uninstalled_at":input.uninstalled_at.to_rfc3339()})),
            };
            Ok::<_, HandlerError>(Json(AuditEvent { id: ghinvite_core::AuditEventId::new(), account_id, occurred_at: chrono::Utc::now(), event_type,
                actor_kind: if actor_id.is_some() { ActorKind::User } else { ActorKind::Github }, actor_id, target_kind: TargetKind::Installation,
                target_id: id.to_string(), metadata, request_id: Some(ctx.invocation_id().to_string()) }))
        }).name("retain_installation_audit").await?;
        ctx.run(|| async {
            match &input {
                InstallationProjection::Onboard { input } => {
                    let account = Account {
                        installation_id: input.installation_id,
                        account_id: input.account_id,
                        account_login: input.account_login.clone(),
                        account_type: input.account_type,
                        installed_at: input.installed_at,
                        uninstalled_at: None,
                        selected_repos: input.selected_repos.clone(),
                    };
                    match self.state.storage.insert_installation(&account).await {
                        Ok(()) => (),
                        Err(ghinvite_core::storage::Error::Conflict(
                            ghinvite_core::storage::ConflictKind::DuplicateId,
                        )) => {
                            if self
                                .state
                                .storage
                                .get_installation(input.installation_id)
                                .await?
                                .as_ref()
                                != Some(&account)
                            {
                                return Err(TerminalError::new_with_code(
                                    409,
                                    "installation projection conflict",
                                )
                                .into());
                            }
                        }
                        Err(error) => return Err(HandlerError::from(error)),
                    }
                }
                InstallationProjection::Repos { input } => {
                    self.state
                        .storage
                        .update_installation_repos(input.installation_id, &input.selected_repos)
                        .await?;
                }
                InstallationProjection::Uninstall { input } => {
                    match self
                        .state
                        .storage
                        .mark_installation_uninstalled(input.installation_id, input.uninstalled_at)
                        .await
                    {
                        Ok(()) | Err(ghinvite_core::storage::Error::NotFound) => (),
                        Err(error) => return Err(HandlerError::from(error)),
                    }
                }
            }
            Ok::<_, HandlerError>(())
        })
        .name("project_installation")
        .await?;
        ctx.run(|| async {
            self.state
                .storage
                .audit(&event)
                .await
                .map_err(HandlerError::from)
        })
        .name("project_installation_audit")
        .await
    }
}
async fn project(
    ctx: &ObjectContext<'_>,
    input: InstallationProjection,
) -> Result<(), TerminalError> {
    ctx.object_client::<InstallationProjectionV1Client>(ctx.key())
        .apply(Json(input))
        .send()
        .await?;
    Ok(())
}

impl AccountInstallationV1Impl {
    async fn load(&self, ctx: &ObjectContext<'_>) -> Result<InstallationStatus, TerminalError> {
        if let Some(Json(status)) = ctx.get("installation/v1").await? {
            return Ok(status);
        }
        let account_id: u64 = ctx.key().parse().map_err(|_| invalid())?;
        if account_id == 0 {
            return Err(invalid());
        }
        // One-time adoption of existing installation facts, not link projections.
        let Json(account) = ctx
            .run(|| async {
                self.state
                    .storage
                    .get_active_installation_by_account_id(account_id)
                    .await
                    .map(Json)
                    .map_err(|_| {
                        HandlerError::from(TerminalError::new_with_code(
                            503,
                            "installation adoption unavailable",
                        ))
                    })
            })
            .name("adopt_installation")
            .await?;
        let status = InstallationStatus {
            account,
            observation: Observation::Unknown,
        };
        ctx.set("installation/v1", Json(status.clone()));
        Ok(status)
    }

    async fn observe(&self, account: &Account) -> Observation {
        match self
            .state
            .github
            .get_installation(account.installation_id)
            .await
        {
            Ok(installation)
                if installation.id == account.installation_id
                    && installation.account.id == account.account_id
                    && installation.suspended_at.is_none() => {}
            Ok(_) => return Observation::Unavailable,
            Err(error) if error.status() == Some(404) => return Observation::Unavailable,
            Err(_) => return Observation::Unknown,
        }
        match self
            .state
            .github
            .all_installation_repo_ids(account.installation_id)
            .await
        {
            Ok(repo_ids) => Observation::Available { repo_ids },
            Err(error) if error.status() == Some(404) => Observation::Unavailable,
            Err(_) => Observation::Unknown,
        }
    }

    async fn refresh_status(
        &self,
        ctx: &ObjectContext<'_>,
        mut status: InstallationStatus,
    ) -> Result<InstallationStatus, TerminalError> {
        if let Some(account) = &mut status.account {
            let observation = if ctx
                .get::<bool>(&format!("retired/{}", account.installation_id))
                .await?
                .is_some()
            {
                Observation::Unavailable
            } else {
                let Json(observation) = ctx
                    .run(|| async { Ok::<_, HandlerError>(Json(self.observe(account).await)) })
                    .name("refresh_github_scope")
                    .await?;
                observation
            };
            // Delivery's existing local prerequisite check must also fail closed.
            let selected = match &observation {
                Observation::Available { repo_ids } => SelectedRepos::Subset(repo_ids.clone()),
                _ => SelectedRepos::Subset(vec![]),
            };
            if account.selected_repos != selected {
                project(
                    ctx,
                    InstallationProjection::Repos {
                        input: crate::installation::ReposChangedInput {
                            installation_id: account.installation_id,
                            selected_repos: selected.clone(),
                        },
                    },
                )
                .await?;
                account.selected_repos = selected;
            }
            if !matches!(observation, Observation::Available { .. })
                && ctx.get::<bool>("refresh_scheduled").await?.is_none()
            {
                ctx.set("refresh_scheduled", true);
                ctx.object_client::<AccountInstallationV1Client>(ctx.key())
                    .recheck()
                    .send_after(std::time::Duration::from_secs(60))
                    .await?;
            }
            status.observation = observation;
        } else {
            status.observation = Observation::Unavailable;
        }
        ctx.set("installation/v1", Json(status.clone()));
        Ok(status)
    }
}

fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid installation identity")
}

impl AccountInstallationV1 for AccountInstallationV1Impl {
    async fn retry_uninstall(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<UninstallInput>,
    ) -> Result<(), TerminalError> {
        self.uninstall(ctx, Json(input)).await
    }
    async fn recheck(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        ctx.clear("refresh_scheduled");
        let status = self.load(&ctx).await?;
        self.refresh_status(&ctx, status).await?;
        Ok(())
    }
    async fn onboard(
        &self,
        ctx: ObjectContext<'_>,
        Json(mut input): Json<OnboardInput>,
    ) -> Result<(), TerminalError> {
        if ctx.key() != input.account_id.to_string() || input.installation_id == 0 {
            return Err(invalid());
        }
        let mut status = self.load(&ctx).await?;
        if ctx
            .get::<bool>(&format!("retired/{}", input.installation_id))
            .await?
            .is_some()
        {
            return Ok(());
        }
        if status
            .account
            .as_ref()
            .is_some_and(|a| a.installation_id == input.installation_id)
        {
            self.refresh_status(&ctx, status).await?;
            return Ok(());
        }
        let Json(existing) = ctx
            .run(|| async {
                self.state
                    .storage
                    .get_installation(input.installation_id)
                    .await
                    .map(Json)
                    .map_err(|_| {
                        HandlerError::from(TerminalError::new_with_code(
                            503,
                            "installation identity storage unavailable",
                        ))
                    })
            })
            .name("existing_installation_identity")
            .await?;
        if let Some(existing) = existing {
            if existing.account_id != input.account_id {
                return Err(invalid());
            }
            if existing.uninstalled_at.is_some() {
                ctx.set(&format!("retired/{}", input.installation_id), true);
                return Ok(());
            }
        }
        let Json(verified) = ctx
            .run(|| async {
                match self
                    .state
                    .github
                    .get_installation(input.installation_id)
                    .await
                {
                    Ok(value) => Ok::<_, HandlerError>(Json(Some(value))),
                    Err(error) if error.status() == Some(404) => Ok(Json(None)),
                    Err(_) => Err(TerminalError::new_with_code(
                        503,
                        "installation identity could not be confirmed",
                    )
                    .into()),
                }
            })
            .name("verify_onboard_identity")
            .await?;
        let Some(verified) = verified else {
            ctx.set(&format!("retired/{}", input.installation_id), true);
            return Ok(());
        };
        if verified.id != input.installation_id || verified.account.id != input.account_id {
            return Err(invalid());
        }
        if let Some(old) = &status.account {
            // A current replacement is never chosen using mutable login or event time.
            // GitHub must confirm the prior installation identity no longer exists.
            ctx.run(|| async {
                match self
                    .state
                    .github
                    .get_installation(old.installation_id)
                    .await
                {
                    Err(error) if error.status() == Some(404) => Ok(()),
                    _ => Err(HandlerError::from(TerminalError::new_with_code(
                        503,
                        "previous installation is not confirmed obsolete",
                    ))),
                }
            })
            .name("verify_replacement")
            .await?;
            let Json(now) = ctx
                .run(|| async { Ok::<_, HandlerError>(Json(chrono::Utc::now())) })
                .name("replacement_time")
                .await?;
            project(
                &ctx,
                InstallationProjection::Uninstall {
                    input: UninstallInput {
                        installation_id: old.installation_id,
                        uninstalled_at: now,
                    },
                },
            )
            .await?;
            ctx.set(&format!("retired/{}", old.installation_id), true);
        }
        input.account_login = verified.account.login;
        input.account_type = verified
            .account
            .account_type
            .as_deref()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        input.selected_repos = SelectedRepos::Subset(vec![]);
        project(
            &ctx,
            InstallationProjection::Onboard {
                input: input.clone(),
            },
        )
        .await?;
        status.account = Some(Account {
            installation_id: input.installation_id,
            account_id: input.account_id,
            account_login: input.account_login,
            account_type: input.account_type,
            installed_at: input.installed_at,
            uninstalled_at: None,
            selected_repos: input.selected_repos,
        });
        self.refresh_status(&ctx, status).await?;
        Ok(())
    }
    async fn refresh(
        &self,
        ctx: ObjectContext<'_>,
        Json(id): Json<u64>,
    ) -> Result<(), TerminalError> {
        let status = self.load(&ctx).await?;
        if status
            .account
            .as_ref()
            .is_some_and(|a| a.installation_id == id)
        {
            self.refresh_status(&ctx, status).await?;
        }
        Ok(())
    }
    async fn uninstall(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<UninstallInput>,
    ) -> Result<(), TerminalError> {
        ctx.set(&format!("retired/{}", input.installation_id), true);
        let mut status = match self.load(&ctx).await {
            Ok(status) => status,
            Err(_) => {
                ctx.object_client::<AccountInstallationV1Client>(ctx.key())
                    .retry_uninstall(Json(input))
                    .send_after(std::time::Duration::from_secs(60))
                    .await?;
                return Ok(());
            }
        };
        if status
            .account
            .as_ref()
            .is_some_and(|a| a.installation_id == input.installation_id)
        {
            project(&ctx, InstallationProjection::Uninstall { input }).await?;
            status.account = None;
            status.observation = Observation::Unavailable;
            ctx.set("installation/v1", Json(status));
        }
        Ok(())
    }
    async fn status(
        &self,
        ctx: ObjectContext<'_>,
    ) -> Result<Json<InstallationStatus>, TerminalError> {
        self.load(&ctx).await.map(Json)
    }
    async fn eligibility(
        &self,
        ctx: ObjectContext<'_>,
        Json(scope): Json<Scope>,
    ) -> Result<Json<Eligibility>, TerminalError> {
        if ctx.key() != scope.account_id.to_string() || scope.account_id == 0 {
            return Err(invalid());
        }
        let status = self.load(&ctx).await?;
        let status = self.refresh_status(&ctx, status).await?;
        Ok(Json(match status.observation {
            Observation::Unavailable => Eligibility::Unavailable {
                reason: Rejection::InstallationUnavailable,
            },
            Observation::Unknown => Eligibility::Unknown,
            Observation::Available { repo_ids }
                if scope.repo_ids.iter().any(|id| !repo_ids.contains(id)) =>
            {
                Eligibility::Unavailable {
                    reason: Rejection::RepositoryUnavailable,
                }
            }
            Observation::Available { .. } => Eligibility::Available,
        }))
    }
}
