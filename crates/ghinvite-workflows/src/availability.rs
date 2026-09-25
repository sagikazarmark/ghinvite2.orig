//! Account-keyed installation convergence. Never owns link or request history.
use crate::{
    AppState,
    installation::{OnboardInput, UninstallInput},
};
use ghinvite_core::{Account, SelectedRepos, admission::Rejection};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture, SharedObjectContext,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Eligibility {
    Available,
    Unavailable { reason: Rejection },
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AdmissionEvidence {
    pub eligibility: Eligibility,
    pub valid_until: chrono::DateTime<chrono::Utc>,
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
    /// When GitHub was read for `observation`. Absent before observation
    /// and after retirement by uninstall.
    #[serde(default)]
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One reading of an installation's availability, and when it was taken.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Observed {
    observation: Observation,
    at: chrono::DateTime<chrono::Utc>,
}

pub struct AccountInstallation {
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
pub struct InstallationProjection {
    pub state: AppState,
}

#[restate_sdk::object]
impl InstallationProjection {
    #[handler]
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

impl AccountInstallation {
    async fn load(&self, ctx: &ObjectContext<'_>) -> Result<InstallationStatus, TerminalError> {
        if let Some(Json(status)) = ctx.get("installation").await? {
            return Ok(status);
        }
        let account_id: u64 = ctx.key().parse().map_err(|_| invalid())?;
        if account_id == 0 {
            return Err(invalid());
        }
        Ok(InstallationStatus {
            account: None,
            observation: Observation::Unknown,
            observed_at: None,
        })
    }

    async fn observe(&self, account: &Account) -> Observation {
        match self
            .state
            .github
            .get_installation(account.installation_id)
            .await
        {
            Ok(Some(installation))
                if installation.id == account.installation_id
                    && installation.account.id == account.account_id
                    && installation.suspended_at.is_none() => {}
            Ok(_) => return Observation::Unavailable,
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
            let retired = ctx
                .get::<bool>(&retired_key(account.installation_id))
                .await?
                .is_some();
            // The reading and the time it was taken are one journal entry, so
            // a replay never dates an observation by when it was replayed.
            let Json(observed) = ctx
                .run(|| async {
                    let observation = if retired {
                        Observation::Unavailable
                    } else {
                        self.observe(account).await
                    };
                    Ok::<_, HandlerError>(Json(Observed {
                        observation,
                        at: chrono::Utc::now(),
                    }))
                })
                .name("refresh_github_scope")
                .await?;
            let Observed { observation, at } = observed;
            let refresh = Refresh::of(account, &observation);
            if let Some(selected) = refresh.selected_repos {
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
            if refresh.recheck && ctx.get::<bool>(RECHECK_SLOT).await?.is_none() {
                ctx.set(RECHECK_SLOT, true);
                ctx.object_client::<AccountInstallationClient>(ctx.key())
                    .recheck()
                    .send_after(RECHECK_INTERVAL)
                    .await?;
            }
            status.observation = observation;
            status.observed_at = Some(at);
        } else {
            status.observation = Observation::Unavailable;
            status.observed_at = None;
        }
        ctx.set("installation", Json(status.clone()));
        Ok(status)
    }

    /// Retire the exact installation even if its onboarding has not arrived.
    async fn continue_uninstall(
        &self,
        ctx: &ObjectContext<'_>,
        input: UninstallInput,
    ) -> Result<(), TerminalError> {
        let mut status = self.load(ctx).await?;
        ctx.set(&retired_key(input.installation_id), true);
        if status
            .account
            .as_ref()
            .is_some_and(|a| a.installation_id == input.installation_id)
        {
            project(ctx, InstallationChange::Uninstall { input }).await?;
            status.account = None;
            status.observation = Observation::Unavailable;
            status.observed_at = None;
            ctx.set("installation", Json(status));
        }
        Ok(())
    }
}

/// The state key marking an installation identity retired. A retired
/// identity is never onboarded again and observes as unavailable.
fn retired_key(installation_id: u64) -> String {
    format!("retired/{installation_id}")
}

/// What a refresh does with an observation of the current installation.
#[derive(Debug)]
struct Refresh {
    /// The available repositories to project, when they differ from what
    /// `account` already selects.
    selected_repos: Option<SelectedRepos>,
    /// Whether the observation recheck must be scheduled.
    recheck: bool,
}

impl Refresh {
    fn of(account: &Account, observation: &Observation) -> Self {
        // Delivery's existing local prerequisite check must also fail closed.
        let selected = match observation {
            Observation::Available { repo_ids } => SelectedRepos::Subset(repo_ids.clone()),
            _ => SelectedRepos::Subset(vec![]),
        };
        Self {
            selected_repos: (account.selected_repos != selected).then_some(selected),
            recheck: !matches!(observation, Observation::Available { .. }),
        }
    }
}

/// Whether a link's repository scope may be admitted under `observation`.
fn eligibility(observation: &Observation, scope: &Scope) -> Eligibility {
    match observation {
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
    }
}

/// The answer a retained observation may give on its own, if it may give one.
///
/// Only an acceptance rests on a retained observation, and only while that
/// observation is recent and covers the whole scope. A rejection is a
/// permanently retained receipt that restoration never revisits, so every
/// answer that would reject — and every account with nothing recent to answer
/// from — reads GitHub again instead (ADR 0004).
fn retained_eligibility(
    status: &InstallationStatus,
    scope: &Scope,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Eligibility> {
    let observed_at = status.observed_at?;
    let recent = (now - observed_at)
        .to_std()
        .is_ok_and(|age| age < RECENT_OBSERVATION);
    (recent
        && matches!(
            eligibility(&status.observation, scope),
            Eligibility::Available
        ))
    .then_some(Eligibility::Available)
}

fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid installation identity")
}

/// Steady-state cadence of the durable observation recheck, and the state key
/// reserving its single scheduled invocation.
const RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const RECHECK_SLOT: &str = "refresh_scheduled";

/// How long an observation stays recent enough to admit on its own. Access lost
/// inside this window is not an admission error: it surfaces as blocked
/// delivery, exactly as a change made after any observation does (ADR 0004).
const RECENT_OBSERVATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);

#[restate_sdk::object]
impl AccountInstallation {
    #[handler]
    async fn admission_evidence(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(scope): Json<Scope>,
    ) -> Result<Json<AdmissionEvidence>, TerminalError> {
        if ctx.key() != scope.account_id.to_string() || scope.account_id == 0 {
            return Err(invalid());
        }
        if let Some(Json(status)) = ctx.get::<Json<InstallationStatus>>("installation").await? {
            let Json(now) = ctx
                .run(|| async { Ok::<_, HandlerError>(Json(chrono::Utc::now())) })
                .name("evidence_time")
                .await?;
            if let Some(answer) = retained_eligibility(&status, &scope, now) {
                return Ok(Json(AdmissionEvidence {
                    eligibility: answer,
                    valid_until: status.observed_at.unwrap() + chrono::Duration::minutes(5),
                }));
            }
        }
        ctx.object_client::<AccountInstallationClient>(ctx.key())
            .observe_admission_evidence(Json(scope))
            .call()
            .await
    }

    #[handler]
    async fn observe_admission_evidence(
        &self,
        ctx: ObjectContext<'_>,
        Json(scope): Json<Scope>,
    ) -> Result<Json<AdmissionEvidence>, TerminalError> {
        if ctx.key() != scope.account_id.to_string() || scope.account_id == 0 {
            return Err(invalid());
        }
        if ctx
            .get::<Json<InstallationStatus>>("installation")
            .await?
            .is_none()
        {
            return Err(TerminalError::new_with_code(
                503,
                "installation authority unavailable",
            ));
        }
        let status = self.load(&ctx).await?;
        let status = self.refresh_status(&ctx, status).await?;
        let Json(now) = ctx
            .run(|| async { Ok::<_, HandlerError>(Json(chrono::Utc::now())) })
            .name("evidence_time")
            .await?;
        let answer = eligibility(&status.observation, &scope);
        // Negative answers are never cached for admission. Allow only the short
        // live handoff; an interrupted caller must obtain a new observation.
        let allowance = if matches!(answer, Eligibility::Available) {
            chrono::Duration::minutes(5)
        } else {
            chrono::Duration::seconds(1)
        };
        Ok(Json(AdmissionEvidence {
            eligibility: answer,
            valid_until: status.observed_at.unwrap_or(now) + allowance,
        }))
    }
    /// Small, retained-only delivery context. Missing authority is uncertainty;
    /// a known uninstall is a known missing prerequisite. Neither reads SQL.
    #[handler]
    async fn current_installation(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<Account>>, TerminalError> {
        let Json(status) = ctx
            .get::<Json<InstallationStatus>>("installation")
            .await?
            .ok_or_else(|| {
                TerminalError::new_with_code(503, "retained installation context unavailable")
            })?;
        Ok(Json(status.account))
    }

    #[handler]
    async fn recheck(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        ctx.clear(RECHECK_SLOT);
        if ctx
            .get::<Json<InstallationStatus>>("installation")
            .await?
            .is_none()
        {
            return Ok(());
        }
        let status = self.load(&ctx).await?;
        self.refresh_status(&ctx, status).await?;
        Ok(())
    }
    #[handler]
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
            .get::<bool>(&retired_key(input.installation_id))
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
        let Json(verified) = ctx
            .run(|| async {
                match self
                    .state
                    .github
                    .get_installation(input.installation_id)
                    .await
                {
                    Ok(value) => Ok::<_, HandlerError>(Json(value)),
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
            ctx.set(&retired_key(input.installation_id), true);
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
                    Ok(None) => Ok(()),
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
            ctx.set(&retired_key(old.installation_id), true);
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
    #[handler]
    async fn refresh(
        &self,
        ctx: ObjectContext<'_>,
        Json(id): Json<u64>,
    ) -> Result<(), TerminalError> {
        let status = self.load(&ctx).await?;
        if status
            .account
            .as_ref()
            .is_some_and(|account| account.installation_id == id)
        {
            self.refresh_status(&ctx, status).await?;
        }
        Ok(())
    }
    #[handler]
    async fn uninstall(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<UninstallInput>,
    ) -> Result<(), TerminalError> {
        self.continue_uninstall(&ctx, input).await
    }
    #[handler]
    async fn status(
        &self,
        ctx: ObjectContext<'_>,
    ) -> Result<Json<InstallationStatus>, TerminalError> {
        self.load(&ctx).await.map(Json)
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

    async fn observed_installation(response: ghinvite_github::mocks::Expectation) -> Observation {
        let mock = ghinvite_github::mocks::MockTransport::scripted(vec![response]);
        let installation = AccountInstallation {
            state: crate::test_support::fixture_state_with_transport(std::sync::Arc::new(
                mock.clone(),
            ))
            .await,
        };
        let account = Account {
            installation_id: 9,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        };
        let observation = installation.observe(&account).await;
        mock.assert_exhausted();
        observation
    }

    #[tokio::test]
    async fn an_installation_github_no_longer_holds_is_unavailable() {
        let observation = observed_installation(ghinvite_github::mocks::Expectation::status(
            ghinvite_github::Method::Get,
            "https://api.github.test/app/installations/9",
            404,
        ))
        .await;
        assert!(
            matches!(observation, Observation::Unavailable),
            "{observation:?}"
        );
    }

    #[tokio::test]
    async fn an_installation_github_would_not_answer_for_is_unknown() {
        let observation = observed_installation(ghinvite_github::mocks::Expectation::status(
            ghinvite_github::Method::Get,
            "https://api.github.test/app/installations/9",
            502,
        ))
        .await;
        assert!(
            matches!(observation, Observation::Unknown),
            "{observation:?}"
        );
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

    fn account_selecting(selected_repos: SelectedRepos) -> Account {
        Account {
            installation_id: 1,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos,
        }
    }

    fn available(repo_ids: &[u64]) -> Observation {
        Observation::Available {
            repo_ids: repo_ids.to_vec(),
        }
    }

    #[test]
    fn an_unchanged_available_scope_projects_nothing_and_needs_no_recheck() {
        let account = account_selecting(SelectedRepos::Subset(vec![10, 11]));

        let refresh = Refresh::of(&account, &available(&[10, 11]));

        assert_eq!(refresh.selected_repos, None);
        assert!(!refresh.recheck);
    }

    #[test]
    fn a_changed_available_scope_projects_what_github_reports() {
        for before in [SelectedRepos::All, SelectedRepos::Subset(vec![10])] {
            let refresh = Refresh::of(&account_selecting(before.clone()), &available(&[10, 11]));

            assert_eq!(
                refresh.selected_repos,
                Some(SelectedRepos::Subset(vec![10, 11])),
                "before={before:?}"
            );
            assert!(!refresh.recheck);
        }
    }

    #[test]
    fn an_installation_not_known_available_selects_nothing_and_rechecks() {
        // Delivery's local prerequisite check reads the projected selection,
        // so anything short of a confirmed scope must fail closed.
        for observation in [Observation::Unavailable, Observation::Unknown] {
            let account = account_selecting(SelectedRepos::Subset(vec![10]));

            let refresh = Refresh::of(&account, &observation);

            assert_eq!(
                refresh.selected_repos,
                Some(SelectedRepos::Subset(vec![])),
                "observation={observation:?}"
            );
            assert!(refresh.recheck, "observation={observation:?}");
        }
    }

    #[test]
    fn an_already_closed_scope_still_rechecks_without_projecting_again() {
        let account = account_selecting(SelectedRepos::Subset(vec![]));

        let refresh = Refresh::of(&account, &Observation::Unavailable);

        assert_eq!(refresh.selected_repos, None);
        assert!(refresh.recheck);
    }

    fn scope(repo_ids: &[u64]) -> Scope {
        Scope {
            account_id: 100,
            repo_ids: repo_ids.to_vec(),
        }
    }

    #[test]
    fn eligibility_follows_the_observation_and_the_whole_scope() {
        let cases = [
            (
                Observation::Unavailable,
                Eligibility::Unavailable {
                    reason: Rejection::InstallationUnavailable,
                },
            ),
            (Observation::Unknown, Eligibility::Unknown),
            (available(&[10, 11]), Eligibility::Available),
            (
                available(&[10]),
                Eligibility::Unavailable {
                    reason: Rejection::RepositoryUnavailable,
                },
            ),
        ];
        for (observation, expected) in cases {
            assert_eq!(
                eligibility(&observation, &scope(&[10, 11])),
                expected,
                "observation={observation:?}"
            );
        }
    }

    /// An account whose retained observation was taken at `observed_at`.
    fn observed(observation: Observation, observed_at: Option<&str>) -> InstallationStatus {
        InstallationStatus {
            account: Some(account_selecting(SelectedRepos::Subset(vec![10, 11]))),
            observation,
            observed_at: observed_at.map(dt),
        }
    }

    const NOW: &str = "2026-05-04T12:00:00Z";

    #[test]
    fn a_recent_available_observation_admits_its_whole_scope_on_its_own() {
        let status = observed(available(&[10, 11]), Some("2026-05-04T11:56:00Z"));

        assert_eq!(
            retained_eligibility(&status, &scope(&[10, 11]), dt(NOW)),
            Some(Eligibility::Available)
        );
    }

    #[test]
    fn an_observation_that_no_longer_speaks_for_now_is_read_again() {
        // The window's own edge, anything past it, and — for a clock that went
        // backwards between observation and admission — anything that claims to
        // postdate the admission reading it.
        for at in [
            "2026-05-04T11:55:00Z",
            "2026-05-04T11:54:00Z",
            "2026-05-04T12:00:01Z",
        ] {
            let status = observed(available(&[10, 11]), Some(at));

            assert_eq!(
                retained_eligibility(&status, &scope(&[10, 11]), dt(NOW)),
                None,
                "observed_at={at}"
            );
        }
    }

    #[test]
    fn nothing_that_would_reject_is_answered_from_a_retained_observation() {
        // Every rejection is a permanently retained receipt, so it has to rest
        // on a live read however recent the retained observation is.
        for observation in [
            Observation::Unavailable,
            Observation::Unknown,
            available(&[10]),
        ] {
            let status = observed(observation.clone(), Some(NOW));

            assert_eq!(
                retained_eligibility(&status, &scope(&[10, 11]), dt(NOW)),
                None,
                "observation={observation:?}"
            );
        }
    }

    #[test]
    fn an_account_never_observed_is_read_live() {
        // An undated observation cannot grant admission.
        for observation in [Observation::Unknown, available(&[10, 11])] {
            let status = observed(observation.clone(), None);

            assert_eq!(
                retained_eligibility(&status, &scope(&[10, 11]), dt(NOW)),
                None,
                "observation={observation:?}"
            );
        }
    }

    #[test]
    fn retired_identities_have_one_key_each() {
        assert_eq!(retired_key(1), "retired/1");
        assert_ne!(retired_key(1), retired_key(2));
    }
}
