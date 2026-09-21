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

/// What a refresh follows: the identity an event named, or the periodic
/// observation recheck, which always follows whichever identity is current.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefreshTarget {
    Identity { installation_id: u64 },
    CurrentObservation,
}

impl RefreshTarget {
    /// The state key holding this target's single scheduled continuation.
    fn slot(&self) -> String {
        match self {
            Self::Identity { installation_id } => format!("refresh_retry/{installation_id}"),
            Self::CurrentObservation => RECHECK_SLOT.to_owned(),
        }
    }
}

/// One refresh and every continuation retained for it. `attempt` counts the
/// adoption outages this refresh has already waited out, so the first is zero.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RefreshContinuation {
    pub target: RefreshTarget,
    pub attempt: u32,
}

#[restate_sdk::object]
pub trait AccountInstallation {
    async fn retry_uninstall(input: Json<UninstallInput>) -> Result<(), TerminalError>;
    async fn retry_refresh(input: Json<RefreshContinuation>) -> Result<(), TerminalError>;
    async fn recheck() -> Result<(), TerminalError>;
    async fn onboard(input: Json<OnboardInput>) -> Result<(), TerminalError>;
    async fn refresh(input: Json<u64>) -> Result<(), TerminalError>;
    async fn uninstall(input: Json<UninstallInput>) -> Result<(), TerminalError>;
    async fn status() -> Result<Json<InstallationStatus>, TerminalError>;
    async fn eligibility(input: Json<Scope>) -> Result<Json<Eligibility>, TerminalError>;
}
pub struct AccountInstallationImpl {
    pub state: AppState,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallationChange {
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
pub trait InstallationProjection {
    async fn apply(input: Json<InstallationChange>) -> Result<(), TerminalError>;
}
pub struct InstallationProjectionImpl {
    pub state: AppState,
}
impl InstallationProjection for InstallationProjectionImpl {
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InstallationChange>,
    ) -> Result<(), TerminalError> {
        let account_id: u64 = ctx.key().parse().map_err(|_| invalid())?;
        // Retained before the projection runs, so a retry replays the event the
        // first attempt minted instead of minting a second one for the same
        // transition.
        let Json(event) = ctx
            .run(|| async {
                Ok::<_, HandlerError>(Json(installation_audit(
                    account_id,
                    &input,
                    ghinvite_core::AuditEventId::new(),
                    chrono::Utc::now(),
                    ctx.invocation_id().to_string(),
                )))
            })
            .name("retain_installation_audit")
            .await?;
        ctx.run(|| async {
            project_installation(&self.state, &input)
                .await
                .map_err(HandlerError::from)
        })
        .name("project_installation")
        .await?;
        ctx.run(|| async {
            self.state
                .storage
                .audit(&event)
                .await
                .map_err(|error| HandlerError::from(ProjectionFailure::Storage(error)))
        })
        .name("project_installation_audit")
        .await
    }
}

/// The audit event one installation transition retains, independent of whether
/// storage has taken the transition yet.
fn installation_audit(
    account_id: u64,
    input: &InstallationChange,
    id: ghinvite_core::AuditEventId,
    occurred_at: chrono::DateTime<chrono::Utc>,
    request_id: String,
) -> ghinvite_core::audit::AuditEvent {
    use ghinvite_core::audit::{ActorKind, AuditEvent, EventType, TargetKind};
    let (installation_id, event_type, actor_id, metadata) = match input {
        InstallationChange::Onboard { input } => (
            input.installation_id,
            EventType::InstallationCreated,
            Some(input.actor_user_id),
            serde_json::json!({
                "account_login": input.account_login,
                "account_type": input.account_type.to_string(),
            }),
        ),
        // Always a subset: a refresh retains exact repository IDs even for an
        // installation configured for all repositories, so there is no `all`
        // selection left to report (docs/installation-availability.md).
        InstallationChange::Repos { input } => (
            input.installation_id,
            EventType::InstallationReposChanged,
            None,
            serde_json::json!({"selected_repos_kind": "subset"}),
        ),
        InstallationChange::Uninstall { input } => (
            input.installation_id,
            EventType::InstallationUninstalled,
            None,
            serde_json::json!({"uninstalled_at": input.uninstalled_at.to_rfc3339()}),
        ),
    };
    AuditEvent {
        id,
        account_id,
        occurred_at,
        event_type,
        // Only an installing user names themselves; GitHub owns every other
        // installation transition.
        actor_kind: if actor_id.is_some() {
            ActorKind::User
        } else {
            ActorKind::Github
        },
        actor_id,
        target_kind: TargetKind::Installation,
        target_id: installation_id.to_string(),
        metadata,
        request_id: Some(request_id),
    }
}

/// Why one installation transition did not reach storage.
#[derive(Debug)]
enum ProjectionFailure {
    /// A replayed onboarding names facts the stored installation does not have,
    /// so no retry converges and the projection is refused outright.
    IdentityConflict,
    /// Storage refused the write or could not answer. Which of the two it was
    /// decides whether Restate retries, so the crate's own classification
    /// settles it rather than the SDK's retry-everything default.
    Storage(ghinvite_core::storage::Error),
}

impl From<ProjectionFailure> for HandlerError {
    fn from(failure: ProjectionFailure) -> Self {
        // `Classified` is this crate's error, not the SDK's `HandlerError` this
        // returns: only it knows which storage failures are worth a retry.
        use crate::error::HandlerError as Classified;
        match failure {
            ProjectionFailure::IdentityConflict => {
                TerminalError::new_with_code(409, "installation projection conflict").into()
            }
            ProjectionFailure::Storage(error) => {
                crate::error::to_sdk_handler_error(Classified::Storage(error))
            }
        }
    }
}

/// Take one installation transition into storage. Idempotent: replaying a
/// transition already applied leaves the installation as it stands.
async fn project_installation(
    state: &AppState,
    input: &InstallationChange,
) -> std::result::Result<(), ProjectionFailure> {
    use ghinvite_core::storage::{ConflictKind, Error as StorageError};
    match input {
        InstallationChange::Onboard { input } => {
            let account = Account {
                installation_id: input.installation_id,
                account_id: input.account_id,
                account_login: input.account_login.clone(),
                account_type: input.account_type,
                installed_at: input.installed_at,
                uninstalled_at: None,
                selected_repos: input.selected_repos.clone(),
            };
            match state.storage.insert_installation(&account).await {
                Ok(()) => Ok(()),
                Err(StorageError::Conflict(ConflictKind::DuplicateId)) => {
                    let stored = state
                        .storage
                        .get_installation(input.installation_id)
                        .await
                        .map_err(ProjectionFailure::Storage)?;
                    if stored.as_ref() == Some(&account) {
                        Ok(())
                    } else {
                        Err(ProjectionFailure::IdentityConflict)
                    }
                }
                Err(error) => Err(ProjectionFailure::Storage(error)),
            }
        }
        InstallationChange::Repos { input } => state
            .storage
            .update_installation_repos(input.installation_id, &input.selected_repos)
            .await
            .map_err(ProjectionFailure::Storage),
        InstallationChange::Uninstall { input } => {
            match state
                .storage
                .mark_installation_uninstalled(input.installation_id, input.uninstalled_at)
                .await
            {
                // An installation ghinvite never projected is already as
                // uninstalled as this transition can make it.
                Ok(()) | Err(StorageError::NotFound) => Ok(()),
                Err(error) => Err(ProjectionFailure::Storage(error)),
            }
        }
    }
}

async fn project(ctx: &ObjectContext<'_>, input: InstallationChange) -> Result<(), TerminalError> {
    ctx.object_client::<InstallationProjectionClient>(ctx.key())
        .apply(Json(input))
        .send()
        .await?;
    Ok(())
}

impl AccountInstallationImpl {
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
                    .map_err(|_| HandlerError::from(adoption_unavailable()))
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
                    InstallationChange::Repos {
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
                && ctx.get::<bool>(RECHECK_SLOT).await?.is_none()
            {
                ctx.set(RECHECK_SLOT, true);
                ctx.object_client::<AccountInstallationClient>(ctx.key())
                    .recheck()
                    .send_after(RECHECK_INTERVAL)
                    .await?;
            }
            status.observation = observation;
        } else {
            status.observation = Observation::Unavailable;
        }
        ctx.set("installation/v1", Json(status.clone()));
        Ok(status)
    }

    /// Shared body of `refresh`, `recheck` and every continuation retained for
    /// them. Refresh work is acknowledged by a durable send before it runs, so a
    /// first adoption that cannot read installation storage keeps the work as a
    /// continuation instead of failing it away.
    async fn continue_refresh(
        &self,
        ctx: &ObjectContext<'_>,
        refresh: RefreshContinuation,
    ) -> Result<(), TerminalError> {
        let status = match self.load(ctx).await {
            Ok(status) => status,
            // An unusable key is not an outage; only unreadable storage is
            // worth waiting for.
            Err(error) if error.code() != ADOPTION_UNAVAILABLE => return Err(error),
            Err(_) => return self.retain_refresh(ctx, refresh).await,
        };
        match refresh.target {
            RefreshTarget::Identity { installation_id } => {
                // Only a continuation retires its own slot; a fresh event that
                // found storage readable leaves the pending one to finish.
                if refresh.attempt > 0 {
                    ctx.clear(&refresh.target.slot());
                }
                // Duplicate, superseded, and retired identities observe nothing:
                // only the adopted identity may refresh.
                if status
                    .account
                    .as_ref()
                    .is_none_or(|account| account.installation_id != installation_id)
                {
                    return Ok(());
                }
            }
            // The recheck's slot is consumed by the invocation it scheduled.
            RefreshTarget::CurrentObservation => ctx.clear(RECHECK_SLOT),
        }
        self.refresh_status(ctx, status).await?;
        Ok(())
    }

    /// Retains one durable continuation per target. A fresh event joins the
    /// continuation already scheduled for that identity rather than starting a
    /// competing chain; the recheck's own slot is already reserved for it.
    async fn retain_refresh(
        &self,
        ctx: &ObjectContext<'_>,
        refresh: RefreshContinuation,
    ) -> Result<(), TerminalError> {
        let slot = refresh.target.slot();
        if matches!(refresh.target, RefreshTarget::Identity { .. })
            && refresh.attempt == 0
            && ctx.get::<bool>(&slot).await?.is_some()
        {
            return Ok(());
        }
        ctx.set(&slot, true);
        ctx.object_client::<AccountInstallationClient>(ctx.key())
            .retry_refresh(Json(RefreshContinuation {
                target: refresh.target,
                attempt: refresh.attempt.saturating_add(1),
            }))
            .send_after(refresh_backoff(refresh.attempt))
            .await?;
        Ok(())
    }
}

fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid installation identity")
}

/// Installation storage could not be read, so adoption is unknown rather than
/// absent. Synchronous callers fail promptly with this; acknowledged refresh
/// work retains a continuation instead.
const ADOPTION_UNAVAILABLE: u16 = 503;

fn adoption_unavailable() -> TerminalError {
    TerminalError::new_with_code(ADOPTION_UNAVAILABLE, "installation adoption unavailable")
}

/// Steady-state cadence of the durable observation recheck, and the state key
/// reserving its single scheduled invocation.
const RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const RECHECK_SLOT: &str = "refresh_scheduled";

/// Bounded backoff for a retained refresh continuation: short first retries so a
/// brief storage outage converges quickly, settling on the recheck cadence so a
/// long one costs no more than the recheck it already runs.
fn refresh_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1 << attempt.min(6)).min(RECHECK_INTERVAL)
}

impl AccountInstallation for AccountInstallationImpl {
    async fn retry_uninstall(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<UninstallInput>,
    ) -> Result<(), TerminalError> {
        self.uninstall(ctx, Json(input)).await
    }
    async fn retry_refresh(
        &self,
        ctx: ObjectContext<'_>,
        Json(refresh): Json<RefreshContinuation>,
    ) -> Result<(), TerminalError> {
        self.continue_refresh(&ctx, refresh).await
    }
    async fn recheck(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        self.continue_refresh(
            &ctx,
            RefreshContinuation {
                target: RefreshTarget::CurrentObservation,
                attempt: 0,
            },
        )
        .await
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
                InstallationChange::Uninstall {
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
            InstallationChange::Onboard {
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
        self.continue_refresh(
            &ctx,
            RefreshContinuation {
                target: RefreshTarget::Identity {
                    installation_id: id,
                },
                attempt: 0,
            },
        )
        .await
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
                ctx.object_client::<AccountInstallationClient>(ctx.key())
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
            project(&ctx, InstallationChange::Uninstall { input }).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_state, fixture_state_with_storage};
    use ghinvite_core::AccountType;
    use ghinvite_core::audit::{ActorKind, EventType, TargetKind};
    use ghinvite_core::storage::{ConflictKind, Error as StorageError};

    fn onboarding() -> InstallationChange {
        InstallationChange::Onboard {
            input: onboarding_input(),
        }
    }

    fn onboarding_input() -> OnboardInput {
        OnboardInput {
            installation_id: 1,
            actor_user_id: 7,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            selected_repos: SelectedRepos::All,
            installed_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    #[tokio::test]
    async fn onboarding_writes_the_installation_it_names() {
        let state = fixture_state().await;

        project_installation(&state, &onboarding()).await.unwrap();

        let account = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(account.account_id, 100);
        assert_eq!(account.account_login, "acme");
        assert!(account.uninstalled_at.is_none());
    }

    #[tokio::test]
    async fn replayed_onboarding_keeps_the_installation_it_already_wrote() {
        let state = fixture_state().await;
        let input = onboarding();
        project_installation(&state, &input).await.unwrap();

        project_installation(&state, &input)
            .await
            .expect("a replayed onboarding is the same onboarding");

        let account = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(account.installed_at, dt("2026-05-04T12:00:00Z"));
    }

    #[tokio::test]
    async fn onboarding_refuses_an_installation_that_names_different_facts() {
        let state = fixture_state().await;
        project_installation(&state, &onboarding()).await.unwrap();

        let mut renamed = onboarding_input();
        renamed.account_login = "someone-else".into();
        let failure = project_installation(&state, &InstallationChange::Onboard { input: renamed })
            .await
            .expect_err("the same installation cannot have been onboarded twice over");

        assert!(matches!(failure, ProjectionFailure::IdentityConflict));
    }

    #[tokio::test]
    async fn onboarding_a_second_active_installation_for_one_account_conflicts() {
        let state = fixture_state().await;
        project_installation(&state, &onboarding()).await.unwrap();

        let mut second = onboarding_input();
        second.installation_id = 2;
        let failure = project_installation(&state, &InstallationChange::Onboard { input: second })
            .await
            .expect_err("an account has at most one active installation");

        // Retrying cannot converge while the first installation is active, so
        // the crate's own classification has to settle it rather than the SDK's
        // retry-everything default.
        assert!(matches!(
            failure,
            ProjectionFailure::Storage(StorageError::Conflict(
                ConflictKind::DuplicateActiveInstallation
            ))
        ));
    }

    #[tokio::test]
    async fn a_repository_change_replaces_the_selected_repositories() {
        let state = fixture_state().await;
        project_installation(&state, &onboarding()).await.unwrap();

        project_installation(
            &state,
            &InstallationChange::Repos {
                input: crate::installation::ReposChangedInput {
                    installation_id: 1,
                    selected_repos: SelectedRepos::Subset(vec![10, 20]),
                },
            },
        )
        .await
        .unwrap();

        let account = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(account.selected_repos, SelectedRepos::Subset(vec![10, 20]));
    }

    #[tokio::test]
    async fn a_repository_change_for_an_unknown_installation_is_refused() {
        let state = fixture_state().await;

        let failure = project_installation(
            &state,
            &InstallationChange::Repos {
                input: crate::installation::ReposChangedInput {
                    installation_id: 999,
                    selected_repos: SelectedRepos::Subset(vec![10]),
                },
            },
        )
        .await
        .expect_err("repositories cannot change on an installation that was never projected");

        assert!(matches!(
            failure,
            ProjectionFailure::Storage(StorageError::NotFound)
        ));
    }

    #[tokio::test]
    async fn uninstalling_stamps_the_time_the_installation_ended() {
        let state = fixture_state().await;
        project_installation(&state, &onboarding()).await.unwrap();
        let uninstalled_at = dt("2026-05-05T00:00:00Z");

        project_installation(
            &state,
            &InstallationChange::Uninstall {
                input: UninstallInput {
                    installation_id: 1,
                    uninstalled_at,
                },
            },
        )
        .await
        .unwrap();

        let account = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(account.uninstalled_at, Some(uninstalled_at));
    }

    #[tokio::test]
    async fn uninstalling_an_installation_that_was_never_projected_succeeds() {
        let state = fixture_state().await;

        project_installation(
            &state,
            &InstallationChange::Uninstall {
                input: UninstallInput {
                    installation_id: 999,
                    uninstalled_at: dt("2026-05-05T00:00:00Z"),
                },
            },
        )
        .await
        .expect("an installation ghinvite never held cannot be left installed");

        assert!(state.storage.get_installation(999).await.unwrap().is_none());
    }

    fn audit_for(input: &InstallationChange) -> ghinvite_core::audit::AuditEvent {
        installation_audit(
            100,
            input,
            ghinvite_core::AuditEventId::new(),
            dt("2026-05-04T12:00:00Z"),
            "inv-1".into(),
        )
    }

    #[test]
    fn an_onboarding_audit_names_the_user_who_installed() {
        let event = audit_for(&onboarding());

        assert_eq!(event.event_type, EventType::InstallationCreated);
        assert_eq!(event.actor_kind, ActorKind::User);
        assert_eq!(event.actor_id, Some(7));
        assert_eq!(event.target_kind, TargetKind::Installation);
        assert_eq!(event.target_id, "1");
        assert_eq!(event.request_id.as_deref(), Some("inv-1"));
        assert_eq!(
            event.metadata.get("account_login"),
            Some(&serde_json::json!("acme"))
        );
        assert_eq!(
            event.metadata.get("account_type"),
            Some(&serde_json::json!("Organization"))
        );
    }

    #[test]
    fn a_repository_change_audit_is_attributed_to_github() {
        let event = audit_for(&InstallationChange::Repos {
            input: crate::installation::ReposChangedInput {
                installation_id: 1,
                selected_repos: SelectedRepos::Subset(vec![10, 20]),
            },
        });

        assert_eq!(event.event_type, EventType::InstallationReposChanged);
        // No GitHub user asked for this; the refresh observed it.
        assert_eq!(event.actor_kind, ActorKind::Github);
        assert_eq!(event.actor_id, None);
        assert_eq!(event.target_id, "1");
        assert_eq!(
            event.metadata.get("selected_repos_kind"),
            Some(&serde_json::json!("subset"))
        );
    }

    #[test]
    fn an_uninstall_audit_records_when_the_installation_ended() {
        let uninstalled_at = dt("2026-05-05T00:00:00Z");
        let event = audit_for(&InstallationChange::Uninstall {
            input: UninstallInput {
                installation_id: 1,
                uninstalled_at,
            },
        });

        assert_eq!(event.event_type, EventType::InstallationUninstalled);
        assert_eq!(event.actor_kind, ActorKind::Github);
        assert_eq!(event.actor_id, None);
        assert_eq!(event.target_id, "1");
        assert_eq!(
            event.metadata.get("uninstalled_at"),
            Some(&serde_json::json!(uninstalled_at.to_rfc3339()))
        );
    }

    #[tokio::test]
    async fn a_replayed_installation_audit_leaves_one_event_behind() {
        let (state, storage) = fixture_state_with_storage().await;
        project_installation(&state, &onboarding()).await.unwrap();
        let event = installation_audit(
            100,
            &onboarding(),
            ghinvite_core::AuditEventId::new(),
            dt("2026-05-04T12:00:00Z"),
            "inv-1".into(),
        );

        // A retry replays the event the first attempt retained rather than
        // minting a second one, so writing it again has to be the same write.
        // This is what keeps one onboarding to one audit event now that the
        // projection audits unconditionally.
        state.storage.audit(&event).await.unwrap();
        state.storage.audit(&event).await.unwrap();

        let created = storage
            .debug_list_audit(100)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == EventType::InstallationCreated)
            .count();
        assert_eq!(created, 1);
    }

    /// Restate's `HandlerError` keeps its classification private; the error it
    /// renders is the only place a caller reads it back.
    fn rendered(failure: ProjectionFailure) -> String {
        let error = HandlerError::from(failure);
        <HandlerError as AsRef<dyn std::error::Error>>::as_ref(&error).to_string()
    }

    #[test]
    fn a_conflicting_identity_is_refused_rather_than_retried() {
        assert_eq!(
            rendered(ProjectionFailure::IdentityConflict),
            "Terminal error [409]: installation projection conflict"
        );
    }

    #[test]
    fn a_storage_conflict_no_retry_converges_on_is_terminal() {
        let rendered = rendered(ProjectionFailure::Storage(StorageError::Conflict(
            ConflictKind::DuplicateActiveInstallation,
        )));
        assert!(rendered.starts_with("Terminal error"), "{rendered}");
    }

    #[test]
    fn a_storage_outage_is_left_for_restate_to_retry() {
        let rendered = rendered(ProjectionFailure::Storage(StorageError::Database(
            "fixture: connection lost".into(),
        )));
        assert!(rendered.starts_with("Retryable error"), "{rendered}");
    }

    #[test]
    fn refresh_backoff_is_bounded_by_the_recheck_cadence() {
        assert_eq!(refresh_backoff(0), std::time::Duration::from_secs(1));
        assert_eq!(refresh_backoff(1), std::time::Duration::from_secs(2));
        assert_eq!(refresh_backoff(5), std::time::Duration::from_secs(32));
        // A long outage never waits longer than the recheck it already runs,
        // and never overflows the shift for a continuation that outlives it.
        for attempt in [6, 7, 64, u32::MAX] {
            assert_eq!(refresh_backoff(attempt), RECHECK_INTERVAL);
        }
    }
}
