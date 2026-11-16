//! App-scoped retained installation facts. Setup cannot name an installer.
mod audit;
mod deletion;
mod org_observation;
#[cfg(feature = "integration")]
#[derive(Default)]
pub struct InstallationAuditFault {
    pub armed: std::sync::atomic::AtomicBool,
    pub paused: std::sync::atomic::AtomicUsize,
}
use crate::{AppState, installation::UninstallInput};
pub use deletion::WebhookOutcome;
use ghinvite_core::account::{
    AccountInstallationRef, AccountSettings, App, DelegationRefusal, Installation,
    InstallationFacts, InstallationWebhookAction, InstallationWebhookInput, OnboardInput,
    SetDelegationInput,
};
use ghinvite_core::audit::{ActorKind, AuditEvent, EventType, TargetKind};
pub use ghinvite_core::installation_audit::InstallationChange;
use ghinvite_core::installation_audit::{InstallationAuditFact, InstallationAuditKind};
use ghinvite_core::{Account, SelectedRepos, admission::Rejection};
pub use org_observation::OrgAuthority;
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
    /// Transitional account-only response for retained delivery contexts.
    pub account: Option<Account>,
    pub installation: Option<Installation>,
    pub repo_admin_delegation: bool,
    pub observation: Observation,
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl InstallationStatus {
    fn empty() -> Self {
        Self {
            account: None,
            installation: None,
            repo_admin_delegation: false,
            observation: Observation::Unknown,
            observed_at: None,
        }
    }
    fn set_installation(&mut self, value: Installation) {
        self.account = value
            .account
            .uninstalled_at
            .is_none()
            .then(|| value.account.clone());
        self.installation = Some(value);
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Observed {
    observation: Observation,
    at: chrono::DateTime<chrono::Utc>,
}
pub struct AccountInstallation {
    pub state: AppState,
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
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        input.events(key).map_err(|_| invalid())?;
        ctx.run(|| async {
            self.state
                .storage
                .project_installation_change(key, &input)
                .await
                .map_err(storage_error)
        })
        .name("project_installation")
        .await
    }
}
#[cfg(test)]
async fn project_installation(
    state: &AppState,
    input: &InstallationChange,
) -> Result<(), HandlerError> {
    let owner = AccountInstallationRef {
        app: input
            .installation
            .as_ref()
            .map(|v| v.app)
            .unwrap_or(App::Repo),
        account_id: input
            .installation
            .as_ref()
            .map(|v| v.account.account_id)
            .or_else(|| input.event.as_ref().map(|e| e.account_id))
            .expect("test identity"),
    };
    state
        .storage
        .project_installation_change(owner, input)
        .await
        .map_err(storage_error)
}
fn storage_error(error: ghinvite_core::storage::Error) -> HandlerError {
    crate::error::to_sdk_handler_error(crate::error::HandlerError::Storage(error))
}
/// The `ghinvite_github` slot of an installed app.
fn github_slot(app: App) -> ghinvite_github::AppSlot {
    match app {
        App::Repo => ghinvite_github::AppSlot::Repo,
        App::Org => ghinvite_github::AppSlot::Org,
    }
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid installation identity")
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "installation operation conflict")
}
fn next_revision(current: u64) -> Result<u64, TerminalError> {
    current
        .checked_add(1)
        .filter(|value| *value <= ghinvite_core::storage::integer::MAX_EXACT)
        .ok_or_else(|| TerminalError::new("installation revision exhausted"))
}
fn retired_key(id: u64) -> String {
    format!("retired/{id}")
}
const RECHECK_SLOT: &str = "recheck_scheduled";
const RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Why the app-authenticated installation read has no answer.
enum InstallationReadFailure {
    /// Refused here or by GitHub: the app is unavailable, or the answer names
    /// another installation.
    Refused(TerminalError),
    /// The read failed; `transient` when a retry may succeed.
    Failed { transient: bool },
}

impl AccountInstallation {
    async fn load(&self, ctx: &ObjectContext<'_>) -> Result<InstallationStatus, TerminalError> {
        let _: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        audit::flush(ctx).await?;
        Ok(audit::status(ctx)
            .await?
            .map(|Json(v)| v)
            .unwrap_or_else(InstallationStatus::empty))
    }
    fn apps(&self) -> Result<&crate::state::RepositoryGithub, TerminalError> {
        self.state
            .github_apps
            .as_deref()
            .ok_or_else(|| TerminalError::new_with_code(503, "installed app runtime unavailable"))
    }
    /// The installed app for `app`.
    fn github_app(&self, app: App) -> Result<ghinvite_github::GithubApp, TerminalError> {
        self.apps()?
            .apps
            .app(github_slot(app))
            .map_err(|_| invalid())
    }
    /// The installation as GitHub reports it to the app (app-authenticated).
    /// Journaled by `verify_onboard_identity`: the result keeps
    /// `GhAppInstallation`'s serde shape (PD-9).
    async fn github_installation(
        &self,
        app: App,
        id: u64,
    ) -> Result<Option<ghinvite_github::payloads::GhAppInstallation>, TerminalError> {
        self.read_installation(app, id)
            .await
            .map_err(|failure| match failure {
                InstallationReadFailure::Refused(error) => error,
                InstallationReadFailure::Failed { .. } => TerminalError::new_with_code(
                    503,
                    "installation identity could not be confirmed",
                ),
            })
    }
    /// [`Self::github_installation`], telling a failure worth retrying apart.
    async fn read_installation(
        &self,
        app: App,
        id: u64,
    ) -> Result<Option<ghinvite_github::payloads::GhAppInstallation>, InstallationReadFailure> {
        let app = self
            .github_app(app)
            .map_err(InstallationReadFailure::Refused)?;
        let credential = app.app_rest().map_err(|_| {
            InstallationReadFailure::Refused(TerminalError::new_with_code(
                503,
                "app authentication unavailable",
            ))
        })?;
        match ghinvite_github::endpoints::get_installation(&credential.rest(), id).await {
            Ok(value) => {
                if value.id != id || value.app_id != app.id().get() {
                    return Err(InstallationReadFailure::Refused(invalid()));
                }
                Ok(Some(value))
            }
            Err(ghinvite_github::ApiError::NotFound) => Ok(None),
            Err(error) => Err(InstallationReadFailure::Failed {
                // A read is safe to repeat.
                transient: error.is_transient_for_retry_safe(true),
            }),
        }
    }
    async fn observe(&self, app: App, account: &Account) -> Observation {
        match self.github_installation(app, account.installation_id).await {
            Ok(Some(value))
                if value.account.id == account.account_id
                    && value.suspended_at.is_none()
                    && (app == App::Repo
                        || (value.account.account_type.as_deref() == Some("Organization")
                            && value.permissions.contains_key("members"))) => {}
            Ok(_) => return Observation::Unavailable,
            Err(_) => return Observation::Unknown,
        }
        if app == App::Org {
            return self.observe_org(account).await;
        }
        let Ok(github) = self.github_app(app) else {
            return Observation::Unknown;
        };
        match github
            .read(
                account.installation_id,
                &ghinvite_github::profiles::REPO_READ,
                |handle| async move {
                    ghinvite_github::endpoints::list_installation_repos(&handle.rest())
                        .await
                        .map_err(ghinvite_github::CallError::<std::convert::Infallible>::Api)
                },
            )
            .await
        {
            Ok(repos) => Observation::Available {
                repo_ids: repos.into_iter().map(|r| r.id).collect(),
            },
            Err(ghinvite_github::TokenCallError::Call(ghinvite_github::CallError::Api(
                ghinvite_github::ApiError::NotFound,
            ))) => Observation::Unavailable,
            Err(_) => Observation::Unknown,
        }
    }
    async fn event(
        ctx: &ObjectContext<'_>,
        installation: Option<Installation>,
        settings: Option<AccountSettings>,
        kind: EventType,
        actor: Option<u64>,
    ) -> Result<AuditEvent, TerminalError> {
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        let at = crate::clock::now(ctx, "installation_event_time").await?;
        let target = installation
            .as_ref()
            .map(|v| format!("{}:{}", v.app.as_str(), v.account.installation_id))
            .unwrap_or_else(|| ctx.key().to_owned());
        let metadata = serde_json::json!({"app":key.app,"revision":installation.as_ref().map(|v|v.revision),"repo_admin_delegation":settings.as_ref().map(|v|v.repo_admin_delegation)});
        let invocation = ctx.invocation_id().to_string();
        let Json(event) = ctx
            .run(move || {
                let target = target.clone();
                let metadata = metadata.clone();
                let invocation = invocation.clone();
                async move {
                    Ok::<_, HandlerError>(Json(AuditEvent {
                        id: ghinvite_core::AuditEventId::new(),
                        account_id: key.account_id,
                        occurred_at: at,
                        event_type: kind,
                        actor_kind: if actor.is_some() {
                            ActorKind::User
                        } else {
                            ActorKind::Github
                        },
                        actor_id: actor,
                        target_kind: TargetKind::Installation,
                        target_id: target,
                        metadata,
                        request_id: Some(invocation),
                    }))
                }
            })
            .name("retain_installation_audit")
            .await?;
        Ok(event)
    }
    async fn project(
        &self,
        ctx: &ObjectContext<'_>,
        installation: Option<Installation>,
        settings: Option<AccountSettings>,
        kind: EventType,
        actor: Option<u64>,
    ) -> Result<(), TerminalError> {
        self.project_receipted(ctx, installation, settings, kind, actor, None)
            .await
    }
    async fn project_receipted(
        &self,
        ctx: &ObjectContext<'_>,
        installation: Option<Installation>,
        settings: Option<AccountSettings>,
        kind: EventType,
        actor: Option<u64>,
        receipt: Option<(String, serde_json::Value)>,
    ) -> Result<(), TerminalError> {
        let event = Self::event(ctx, installation.clone(), settings.clone(), kind, actor).await?;
        let mut status = audit::status(ctx)
            .await?
            .map(|v| v.0)
            .unwrap_or_else(InstallationStatus::empty);
        if let Some(settings) = &settings {
            status.repo_admin_delegation = settings.repo_admin_delegation;
        }
        let mut facts = vec![];
        if let Some(value) = &installation {
            if let Some(suspended_at) = value.suspended_at
                && status
                    .installation
                    .as_ref()
                    .filter(|old| old.account.installation_id == value.account.installation_id)
                    .and_then(|old| old.suspended_at)
                    != Some(suspended_at)
            {
                facts.push(InstallationAuditFact::V1 {
                    revision: value.revision,
                    fact: InstallationAuditKind::Suspended { suspended_at },
                });
            }
            status.set_installation(value.clone());
        }
        audit::commit(
            ctx,
            &self.state,
            status,
            InstallationChange {
                installation,
                settings,
                event: Some(event),
                audit_facts: facts,
            },
            None,
            receipt,
        )
        .await
    }
    async fn refresh_status(
        &self,
        ctx: &ObjectContext<'_>,
        mut status: InstallationStatus,
    ) -> Result<InstallationStatus, TerminalError> {
        if let Some(mut value) = status
            .installation
            .clone()
            .filter(|v| v.account.uninstalled_at.is_none())
        {
            let retired = ctx
                .get::<bool>(&retired_key(value.account.installation_id))
                .await?
                .is_some();
            let Json(observed) = ctx
                .run(|| async {
                    Ok::<_, HandlerError>(Json(Observed {
                        observation: if retired {
                            Observation::Unavailable
                        } else {
                            self.observe(value.app, &value.account).await
                        },
                        at: chrono::Utc::now(),
                    }))
                })
                .name("refresh_github_scope")
                .await?;
            let refresh = Refresh::of(&value.account, &observed.observation);
            if let Some(selected) = refresh.selected_repos {
                value.account.selected_repos = selected;
                value.revision = next_revision(value.revision)?;
                self.project(
                    ctx,
                    Some(value.clone()),
                    None,
                    EventType::InstallationReposChanged,
                    None,
                )
                .await?;
                status.set_installation(value);
            }
            if refresh.recheck && ctx.get::<bool>(RECHECK_SLOT).await?.is_none() {
                ctx.set(RECHECK_SLOT, true);
                ctx.object_client::<AccountInstallationClient>(ctx.key())
                    .recheck()
                    .send_after(RECHECK_INTERVAL)
                    .await?;
            }
            status.observation = observed.observation;
            status.observed_at = Some(observed.at);
        } else {
            status.observation = Observation::Unavailable;
            status.observed_at = None;
        }
        audit::save_status(ctx, status.clone()).await?;
        Ok(status)
    }
    async fn continue_uninstall(
        &self,
        ctx: &ObjectContext<'_>,
        input: UninstallInput,
    ) -> Result<(), TerminalError> {
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.app != input.app {
            return Err(invalid());
        }
        let mut status = self.load(ctx).await?;
        ctx.set(&retired_key(input.installation_id), true);
        if let Some(mut value) = status.installation.clone().filter(|v| {
            v.account.installation_id == input.installation_id && v.account.uninstalled_at.is_none()
        }) {
            value.account.uninstalled_at = Some(input.uninstalled_at);
            value.revision = next_revision(value.revision)?;
            self.project(
                ctx,
                Some(value.clone()),
                None,
                EventType::InstallationUninstalled,
                None,
            )
            .await?;
            status.set_installation(value);
            status.observation = Observation::Unavailable;
            status.observed_at = None;
            audit::save_status(ctx, status).await?;
        }
        Ok(())
    }
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
/// How long an observation stays recent enough to admit on its own. Access lost
/// inside this window is not an admission error: it surfaces as blocked
/// delivery, exactly as a change made after any observation does (ADR 0004).
const RECENT_OBSERVATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);

impl AccountInstallation {
    async fn adopt(
        &self,
        ctx: &ObjectContext<'_>,
        facts: InstallationFacts,
        refresh_scope: bool,
    ) -> Result<InstallationStatus, TerminalError> {
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.app != facts.app
            || key.account_id != facts.account.account_id
            || facts.account.installation_id == 0
        {
            return Err(invalid());
        }
        let mut status = self.load(ctx).await?;
        if ctx
            .get::<bool>(&retired_key(facts.account.installation_id))
            .await?
            .is_some()
        {
            return Ok(status);
        }
        let Json(verified) = ctx
            .run(|| async {
                self.github_installation(facts.app, facts.account.installation_id)
                    .await
                    .map(Json)
                    .map_err(HandlerError::from)
            })
            .name("verify_onboard_identity")
            .await?;
        let Some(verified) = verified else {
            let at = crate::clock::now(ctx, "uninstall_time").await?;
            self.continue_uninstall(
                ctx,
                UninstallInput {
                    app: facts.app,
                    installation_id: facts.account.installation_id,
                    uninstalled_at: at,
                },
            )
            .await?;
            return self.load(ctx).await;
        };
        if verified.account.id != facts.account.account_id {
            return Err(invalid());
        }
        let previous = status.installation.clone();
        if let Some(mut old) = previous.clone().filter(|v| {
            v.account.installation_id != facts.account.installation_id
                && v.account.uninstalled_at.is_none()
        }) {
            ctx.run(|| async {
                match self
                    .github_installation(old.app, old.account.installation_id)
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
            old.account.uninstalled_at = Some(crate::clock::now(ctx, "replacement_time").await?);
            old.revision = next_revision(old.revision)?;
            self.project(
                ctx,
                Some(old.clone()),
                None,
                EventType::InstallationUninstalled,
                None,
            )
            .await?;
            ctx.set(&retired_key(old.account.installation_id), true);
        }
        let same = previous
            .as_ref()
            .filter(|v| v.account.installation_id == facts.account.installation_id);
        let mut value = facts.into_snapshot(same.map(|v| v.revision).unwrap_or(0));
        value.account.account_login = verified.account.login;
        value.account.account_type = verified
            .account
            .account_type
            .as_deref()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        value.account.installed_at = verified.created_at;
        value.account.uninstalled_at = None;
        value.account.selected_repos = same
            .map(|v| v.account.selected_repos.clone())
            .unwrap_or(SelectedRepos::Subset(vec![]));
        value.permissions = verified
            .permissions
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    match v {
                        ghinvite_github::payloads::GhInstallationPermission::Read => {
                            ghinvite_core::account::InstallationPermission::Read
                        }
                        ghinvite_github::payloads::GhInstallationPermission::Write => {
                            ghinvite_core::account::InstallationPermission::Write
                        }
                    },
                )
            })
            .collect();
        value.suspended_at = verified.suspended_at;
        if value.suspended_at.is_some() {
            value.account.selected_repos = SelectedRepos::Subset(vec![]);
        }

        if let Some(old) = same {
            value.installed_by_user_id = old.installed_by_user_id;
            value.installed_by_login = old.installed_by_login.clone();
        }
        if same != Some(&value) {
            value.revision = next_revision(value.revision)?;
            self.project(
                ctx,
                Some(value.clone()),
                None,
                if same.is_none() {
                    EventType::InstallationCreated
                } else {
                    EventType::InstallationReposChanged
                },
                None,
            )
            .await?;
        }
        status.set_installation(value);
        audit::save_status(ctx, status.clone()).await?;
        if key.app == App::Repo
            || (key.app == App::Org && self.state.organization_delivery.is_some())
        {
            ctx.object_client::<crate::sweep_schedule::SweepScheduleClient>(ctx.key())
                .ensure_scheduled()
                .send()
                .await?;
        }
        if refresh_scope {
            self.refresh_status(ctx, status).await
        } else {
            Ok(status)
        }
    }
}

#[restate_sdk::object]
impl AccountInstallation {
    #[handler]
    async fn onboard(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<OnboardInput>,
    ) -> Result<(), TerminalError> {
        let receipt = format!("onboard/{}", String::from(input.operation_id.clone()));
        if let Some(Json(old)) = ctx.get::<Json<OnboardInput>>(&receipt).await? {
            return if old == input {
                Ok(())
            } else {
                Err(conflict())
            };
        }
        self.adopt(&ctx, input.verified.clone(), true).await?;
        ctx.set(&receipt, Json(input));
        Ok(())
    }
    /// Answers `None` (an empty body, as the former `()` answer) unless an
    /// `installation.deleted` delivery was not confirmed (PD-8).
    #[handler]
    async fn onboard_webhook(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InstallationWebhookInput>,
    ) -> Result<Option<Json<WebhookOutcome>>, TerminalError> {
        if input.delivery_id.len() != 64
            || !input
                .delivery_id
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            || input.sender_id == 0
            || input.sender_login.is_empty()
            || input.sender_login.len() > 100
            || input.sender_login.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.app != input.verified.app || key.account_id != input.verified.account.account_id {
            return Err(invalid());
        }
        audit::flush(&ctx).await?;
        let receipt = format!("webhook/{}", input.delivery_id);
        let now = self.webhook_time(&ctx).await?;
        if let Some(Json(expires_at)) = ctx
            .get::<Json<chrono::DateTime<chrono::Utc>>>(&receipt)
            .await?
        {
            if now < expires_at {
                return Ok(None);
            }
            ctx.clear(&receipt);
        }
        let installation_id = input.verified.account.installation_id;
        let changes_owner = matches!(
            input.action,
            InstallationWebhookAction::Created | InstallationWebhookAction::NewPermissionsAccepted
        );
        let source = audit::source(&ctx, installation_id).await?;
        let fresh_source = source
            .as_ref()
            .is_none_or(|source| input.updated_at > source.updated_at);
        if changes_owner && let Some(source) = source.as_ref() {
            if input.updated_at < source.updated_at {
                return self
                    .retain_webhook(&ctx, input.delivery_id, now)
                    .await
                    .map(|()| None);
            }
            if input.updated_at == source.updated_at && source.sender_id != input.sender_id {
                return Err(conflict());
            }
        }

        let mut outcome = None;
        if matches!(input.action, InstallationWebhookAction::Deleted) {
            // PD-8: the signed payload alone never retires an installation.
            if let Some(at) = self
                .confirm_deletion(&ctx, key.app, installation_id)
                .await?
            {
                self.continue_uninstall(
                    &ctx,
                    UninstallInput {
                        app: key.app,
                        installation_id,
                        uninstalled_at: at,
                    },
                )
                .await?;
            } else {
                tracing::warn!(
                    app = key.app.as_str(),
                    installation_id,
                    "installation.deleted not confirmed: GitHub still reports the installation; delivery ignored"
                );
                outcome = Some(Json(WebhookOutcome::DeletionNotConfirmed));
            }
        } else if ctx
            .get::<bool>(&retired_key(installation_id))
            .await?
            .is_none()
        {
            let mut status = self.adopt(&ctx, input.verified.clone(), true).await?;
            if let Some(mut value) = status
                .installation
                .clone()
                .filter(|v| v.account.installation_id == installation_id)
            {
                if changes_owner && fresh_source {
                    let owner_changed = value.installed_by_user_id != Some(input.sender_id)
                        || value.installed_by_login.as_deref() != Some(&input.sender_login);
                    let permissions = matches!(
                        input.action,
                        InstallationWebhookAction::NewPermissionsAccepted
                    );
                    let source = audit::SourceWatermark {
                        installation_id,
                        updated_at: input.updated_at,
                        sender_id: input.sender_id,
                    };
                    if owner_changed || permissions {
                        value.installed_by_user_id = Some(input.sender_id);
                        value.installed_by_login = Some(input.sender_login.clone());
                        value.revision = next_revision(value.revision)?;
                        let event = if owner_changed {
                            Some(
                                Self::event(
                                    &ctx,
                                    Some(value.clone()),
                                    None,
                                    EventType::InstallationOwnerRecorded,
                                    Some(input.sender_id),
                                )
                                .await?,
                            )
                        } else {
                            None
                        };
                        let facts = if permissions {
                            vec![InstallationAuditFact::V1 {
                                revision: value.revision,
                                fact: InstallationAuditKind::PermissionsAccepted {
                                    source_updated_at: input.updated_at,
                                    payload_sha256: input.delivery_id.clone(),
                                    sender_id: input.sender_id,
                                },
                            }]
                        } else {
                            vec![]
                        };
                        status.set_installation(value.clone());
                        audit::commit(
                            &ctx,
                            &self.state,
                            status,
                            InstallationChange {
                                installation: Some(value),
                                settings: None,
                                event,
                                audit_facts: facts,
                            },
                            Some(source),
                            None,
                        )
                        .await?;
                    } else {
                        audit::save_source(&ctx, status, source).await?;
                    }
                }
                self.refresh_credentials(&ctx, key.app, installation_id)
                    .await?;
            }
        }
        self.retain_webhook(&ctx, input.delivery_id, now).await?;
        Ok(outcome)
    }
    #[handler]
    async fn expire_webhook(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<WebhookExpiry>,
    ) -> Result<(), TerminalError> {
        let _: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if !valid_payload_hash(&input.payload_sha256) {
            return Err(invalid());
        }
        let key = format!("webhook/{}", input.payload_sha256);
        let current = ctx
            .get::<Json<chrono::DateTime<chrono::Utc>>>(&key)
            .await?
            .map(|Json(v)| v);
        if current != Some(input.expires_at) {
            return Ok(());
        }
        let now = self.webhook_time(&ctx).await?;
        if now >= input.expires_at {
            ctx.clear(&key);
        } else {
            ctx.object_client::<AccountInstallationClient>(ctx.key())
                .expire_webhook(Json(input.clone()))
                .send_after((input.expires_at - now).to_std().map_err(|_| invalid())?)
                .await?;
        }
        Ok(())
    }
    #[handler]
    async fn set_org_cap(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::budget::SetOrgCap>,
    ) -> Result<Json<ghinvite_core::budget::OrgCapOutcome>, TerminalError> {
        crate::account_budget::set(&ctx, &self.state, input)
            .await
            .map(Json)
    }
    #[handler]
    async fn org_cap_policy(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<ghinvite_core::budget::OrgCapView>, TerminalError> {
        let owner: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if owner.app != App::Repo {
            return Err(invalid());
        }
        Ok(Json(ghinvite_core::budget::OrgCapView {
            enabled: self.state.invite_budget.is_some(),
            policy: ctx
                .get::<Json<ghinvite_core::budget::OrgCapPolicy>>(crate::account_budget::POLICY)
                .await?
                .map(|p| p.0),
        }))
    }
    #[handler]
    async fn set_delegation(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<SetDelegationInput>,
    ) -> Result<Json<Result<(), DelegationRefusal>>, TerminalError> {
        audit::flush(&ctx).await?;
        let receipt = format!("delegation/{}", String::from(input.operation_id.clone()));
        if let Some(Json((old, result))) = ctx
            .get::<Json<(SetDelegationInput, Result<(), DelegationRefusal>)>>(&receipt)
            .await?
        {
            return if old == input {
                Ok(Json(result))
            } else {
                Err(conflict())
            };
        }
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        let mut status = self.load(&ctx).await?;
        // One account settings sequence lives at repo:<account>, including
        // org-only installations. The org owner supplies evidence, never writes
        // this setting or calls back into the repo owner.
        let result = if key.app != App::Repo {
            Err(DelegationRefusal::OrgAppRequired)
        } else {
            let Json(authority) = ctx
                .object_client::<AccountInstallationClient>(format!("org:{}", key.account_id))
                .org_authority(Json(input.actor_user_id))
                .call()
                .await?;
            match authority {
                OrgAuthority::Owner => Ok(()),
                OrgAuthority::Unavailable => {
                    return Err(TerminalError::new_with_code(
                        503,
                        "organization authority unavailable",
                    ));
                }
                OrgAuthority::NonOwner | OrgAuthority::Denied => Err(DelegationRefusal::NotOwner),
                OrgAuthority::Absent => match &status.installation {
                    Some(value)
                        if value.app == App::Repo
                            && value.account.account_id == key.account_id
                            && value.account.uninstalled_at.is_none()
                            && value.suspended_at.is_none()
                            && value.account.account_type
                                == ghinvite_core::AccountType::Organization
                            && value.installed_by_user_id == Some(input.actor_user_id) =>
                    {
                        Ok(())
                    }
                    _ => Err(DelegationRefusal::NotOwner),
                },
            }
        };
        if result.is_ok() {
            let old = audit::delegation(&ctx).await?;
            let value = AccountSettings {
                account_id: key.account_id,
                revision: next_revision(old.as_ref().map(|v| v.revision).unwrap_or(0))?,
                repo_admin_delegation: input.on,
                changed_by: input.actor_user_id,
                changed_at: crate::clock::now(&ctx, "delegation_time").await?,
            };
            self.project_receipted(
                &ctx,
                None,
                Some(value.clone()),
                EventType::RepositoryAdminDelegationChanged,
                Some(input.actor_user_id),
                Some((
                    receipt.clone(),
                    serde_json::to_value((&input, &result)).expect("typed receipt"),
                )),
            )
            .await?;
            status.repo_admin_delegation = input.on;
            audit::save_status(&ctx, status).await?;
        }
        ctx.set(&receipt, Json((input, result)));
        Ok(Json(result))
    }
    #[handler]
    async fn recheck_permissions(
        &self,
        ctx: ObjectContext<'_>,
        Json(id): Json<u64>,
    ) -> Result<(), TerminalError> {
        let status = self.load(&ctx).await?;
        if let Some(value) = status
            .installation
            .filter(|v| v.account.installation_id == id && v.account.uninstalled_at.is_none())
        {
            let facts = InstallationFacts {
                app: value.app,
                account: value.account,
                suspended_at: value.suspended_at,
                permissions: value.permissions,
            };
            self.adopt(&ctx, facts, false).await?;
            self.refresh_credentials(&ctx, value.app, id).await?;
        }
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
            .is_some_and(|a| a.installation_id == id)
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
    async fn recheck(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        ctx.clear(RECHECK_SLOT);
        let _: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        let Some(Json(status)) = audit::status(&ctx).await? else {
            // A stale timer has no evidence of an installation or its deletion.
            return Ok(());
        };
        self.refresh_status(&ctx, status).await?;
        Ok(())
    }
    #[handler]
    async fn status(
        &self,
        ctx: ObjectContext<'_>,
    ) -> Result<Json<InstallationStatus>, TerminalError> {
        self.load(&ctx).await.map(Json)
    }
    /// Canonical retained evidence for advisory eligibility. Unlike the
    /// transitional account-only view this includes suspension and permissions.
    #[handler]
    async fn eligibility_installation(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<Installation>>, TerminalError> {
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.app != App::Org {
            return Err(invalid());
        }
        let Json(value) = audit::shared_status(&ctx).await?.ok_or_else(|| {
            TerminalError::new_with_code(503, "retained installation context unavailable")
        })?;
        Ok(Json(value.installation))
    }
    #[handler]
    async fn current_installation(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<Account>>, TerminalError> {
        let _: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        let Json(value) = audit::shared_status(&ctx).await?.ok_or_else(|| {
            TerminalError::new_with_code(503, "retained installation context unavailable")
        })?;
        Ok(Json(value.account))
    }
    /// Read-only org authority. Absent means confirmed no current org app;
    /// suspension, pending membership and unreadable evidence never fall back
    /// to the repository app's historical installer.
    #[handler]
    async fn org_authority(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(user_id): Json<u64>,
    ) -> Result<Json<OrgAuthority>, TerminalError> {
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.app != App::Org || user_id == 0 {
            return Err(invalid());
        }
        let status = audit::shared_status(&ctx).await?;
        let Some(installation) = status.and_then(|Json(s)| s.installation) else {
            return Ok(Json(OrgAuthority::Absent));
        };
        if installation.account.uninstalled_at.is_some() {
            return Ok(Json(OrgAuthority::Absent));
        }
        if installation.app != App::Org || installation.account.account_id != key.account_id {
            return Ok(Json(OrgAuthority::Denied));
        }
        ctx.run(|| async {
            Ok::<_, HandlerError>(Json(self.org_owner_evidence(&installation, user_id).await))
        })
        .name("organization_actor_authority")
        .await
    }
    /// Coarse account boundary only. Trusted ingress still verifies each target.
    /// The cross-owner read is acyclic: org_authority never calls the repo owner.
    #[handler]
    async fn decision_authority(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(user_id): Json<u64>,
    ) -> Result<Json<bool>, TerminalError> {
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.app != App::Repo || user_id == 0 {
            return Ok(Json(false));
        }
        let status = audit::shared_status(&ctx).await?;
        let status = status
            .map(|Json(s)| s)
            .unwrap_or_else(InstallationStatus::empty);
        let live_repo = status.installation.as_ref().filter(|installation| {
            installation.app == App::Repo
                && installation.account.account_id == key.account_id
                && installation.account.uninstalled_at.is_none()
                && installation.suspended_at.is_none()
        });
        if let Some(installation) = live_repo
            && installation.account.account_type == ghinvite_core::AccountType::User
        {
            return Ok(Json(user_id == key.account_id));
        }
        let Json(authority) = ctx
            .object_client::<AccountInstallationClient>(format!("org:{}", key.account_id))
            .org_authority(Json(user_id))
            .call()
            .await?;
        let allowed = match authority {
            OrgAuthority::Owner => true,
            OrgAuthority::Unavailable => {
                return Err(TerminalError::new_with_code(
                    503,
                    "organization authority unavailable",
                ));
            }
            OrgAuthority::Denied => false,
            OrgAuthority::NonOwner => live_repo.is_some() && status.repo_admin_delegation,
            OrgAuthority::Absent => live_repo.is_some_and(|installation| {
                installation.account.account_type == ghinvite_core::AccountType::Organization
                    && (installation.installed_by_user_id == Some(user_id)
                        || status.repo_admin_delegation)
            }),
        };
        Ok(Json(allowed))
    }
    #[handler]
    async fn admission_evidence(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(scope): Json<Scope>,
    ) -> Result<Json<AdmissionEvidence>, TerminalError> {
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.account_id != scope.account_id {
            return Err(invalid());
        }
        if let Some(Json(status)) = audit::shared_status(&ctx).await? {
            let now = crate::clock::now(&ctx, "evidence_time").await?;
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
        let key: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if key.account_id != scope.account_id {
            return Err(invalid());
        }
        if audit::status(&ctx).await?.is_none() {
            return Err(TerminalError::new_with_code(
                503,
                "installation authority unavailable",
            ));
        }
        let status = self.load(&ctx).await?;
        let status = self.refresh_status(&ctx, status).await?;
        let now = crate::clock::now(&ctx, "evidence_time").await?;
        let answer = eligibility(&status.observation, &scope);
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
}
impl AccountInstallation {
    async fn refresh_credentials(
        &self,
        ctx: &ObjectContext<'_>,
        app: App,
        id: u64,
    ) -> Result<(), TerminalError> {
        ctx.run(|| async {
            // adopt/onboard_webhook has retained the refreshed facts and
            // durably queued their monotonic projection. SQL completion must
            // not hold this owner and prevent uninstall/replacement progress.
            let github = self.github_app(app)?;
            // A still-unapproved permission remains a blocked prerequisite. Its
            // negative result is retained; no repeated mint loop is introduced.
            let unavailable = refresh_tokens(&github, app, id).await;
            if unavailable {
                return Err(HandlerError::from(std::io::Error::other(
                    "installation credential refresh unavailable",
                )));
            }
            Ok::<_, HandlerError>(())
        })
        .name("refresh_installation_credentials")
        .await
    }
}

/// Drop every cached token of `app`'s installation `id` and create one for
/// the write scope (gh-toolkit's `invalidate_installation` + `ensure_fresh`;
/// P1 spec §10.2): one creation, the read scope's comes when next used.
/// Whether it failed transiently, the only failure worth retrying.
async fn refresh_tokens(github: &ghinvite_github::GithubApp, app: App, id: u64) -> bool {
    use ghinvite_github::profiles::{ORG_INVITE, REPO_INVITE};
    let scope = match app {
        App::Repo => &REPO_INVITE,
        App::Org => &ORG_INVITE,
    };
    let result = github.refresh(id, scope).await;
    matches!(
        result.as_ref().map_err(ghinvite_github::apps::github_error),
        Err(Some(octotoken::octoauth::Error::Transient { .. }))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::dt;
    use ghinvite_core::AccountType;
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
            installation: None,
            repo_admin_delegation: false,
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

    #[tokio::test]
    async fn credential_refresh_creates_one_write_token_and_retries_only_transient_failures() {
        use ghinvite_github::{
            AppSlot, GithubApps,
            mocks::{FixedTokens, MockTransport},
            profiles::{ORG_INVITE, ORG_READ, REPO_INVITE, REPO_READ},
        };
        use octotoken::{TokenError, octoauth::Error as GitHub};
        let repo = FixedTokens::new(123);
        let org = FixedTokens::new(456);
        let mock = MockTransport::scripted(vec![]);
        let apps = GithubApps::for_tests(
            &mock,
            [(AppSlot::Repo, repo.clone()), (AppSlot::Org, org.clone())],
        );
        let github = |slot| apps.app(slot).unwrap();

        // Cached tokens of every scope, as deliveries leave them.
        for (slot, installation, scopes) in [
            (AppSlot::Repo, 1, [&REPO_INVITE, &REPO_READ]),
            (AppSlot::Org, 2, [&ORG_INVITE, &ORG_READ]),
        ] {
            for scope in scopes {
                github(slot).ensure(installation, scope).await.unwrap();
            }
        }
        assert!(!refresh_tokens(&github(AppSlot::Repo), App::Repo, 1).await);
        assert!(!refresh_tokens(&github(AppSlot::Org), App::Org, 2).await);
        // One creation per refresh: the write scope's (gh-toolkit's
        // `invalidate_installation` + `ensure_fresh`).
        assert_eq!(repo.generation(1, &REPO_INVITE), 2);
        assert_eq!(org.generation(2, &ORG_INVITE), 2);
        assert_eq!(repo.generation(1, &REPO_READ), 1);
        assert_eq!(org.generation(2, &ORG_READ), 1);
        assert_eq!((repo.replace_calls(), org.replace_calls()), (0, 0));
        // The read scopes were invalidated: their next use creates a token.
        github(AppSlot::Org).ensure(2, &ORG_READ).await.unwrap();
        assert_eq!(org.generation(2, &ORG_READ), 2);
        assert_eq!(repo.generation(2, &ORG_READ), 0);

        // A still-refused write scope is retained as is, never retried.
        org.fail_next_token(TokenError::GitHub(GitHub::PermissionsNotGranted));
        assert!(!refresh_tokens(&github(AppSlot::Org), App::Org, 2).await);
        assert_eq!(org.generation(2, &ORG_INVITE), 2);
        // Neither are rate limits retried here (gh-toolkit's `Transient` only).
        org.fail_next_token(TokenError::GitHub(GitHub::RateLimited {
            scope: ghinvite_github::error::RateLimitScope::Secondary,
            retry_after: None,
        }));
        assert!(!refresh_tokens(&github(AppSlot::Org), App::Org, 2).await);
        // A transient failure fails the step.
        org.fail_next_token(TokenError::GitHub(GitHub::Transient {
            kind: octotoken::octoauth::TransientKind::NoResponse,
        }));
        assert!(refresh_tokens(&github(AppSlot::Org), App::Org, 2).await);
        assert!(!refresh_tokens(&github(AppSlot::Org), App::Org, 2).await);
        assert_eq!(org.generation(2, &ORG_INVITE), 3);
        mock.assert_exhausted();
    }

    #[test]
    fn retired_identities_have_one_key_each() {
        assert_eq!(retired_key(1), "retired/1");
        assert_ne!(retired_key(1), retired_key(2));
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use crate::test_support::{dt, fixture_state, fixture_state_with_storage};
    fn transition() -> InstallationChange {
        let at = dt("2026-01-01T00:00:00Z");
        InstallationChange {
            installation: Some(
                InstallationFacts {
                    app: App::Repo,
                    account: Account {
                        installation_id: 1,
                        account_id: 100,
                        account_login: "acme".into(),
                        account_type: ghinvite_core::AccountType::Organization,
                        installed_at: at,
                        uninstalled_at: None,
                        selected_repos: SelectedRepos::Subset(vec![10]),
                    },
                    suspended_at: None,
                    permissions: Default::default(),
                }
                .into_snapshot(1),
            ),
            settings: None,
            audit_facts: vec![],
            event: Some(AuditEvent {
                id: ghinvite_core::AuditEventId::new(),
                account_id: 100,
                occurred_at: at,
                event_type: EventType::InstallationCreated,
                actor_kind: ActorKind::Github,
                actor_id: None,
                target_kind: TargetKind::Installation,
                target_id: "repo:1".into(),
                metadata: serde_json::json!({"app":"repo"}),
                request_id: Some("inv-1".into()),
            }),
        }
    }
    #[tokio::test]
    async fn onboarding_writes_the_verified_installation_it_names() {
        let state = fixture_state().await;
        let change = transition();
        project_installation(&state, &change).await.unwrap();
        let stored = state
            .storage
            .app_installation(App::Repo, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(Some(stored), change.installation);
    }
    #[tokio::test]
    async fn replayed_onboarding_keeps_the_same_snapshot() {
        let state = fixture_state().await;
        let change = transition();
        project_installation(&state, &change).await.unwrap();
        project_installation(&state, &change).await.unwrap();
        assert_eq!(
            state.storage.app_installation(App::Repo, 1).await.unwrap(),
            change.installation
        );
    }
    #[tokio::test]
    async fn equal_revision_changed_identity_is_retained_for_repair() {
        let state = fixture_state().await;
        let mut change = transition();
        project_installation(&state, &change).await.unwrap();
        change.installation.as_mut().unwrap().account.account_login = "different".into();
        let error = project_installation(&state, &change).await.unwrap_err();
        assert!(
            <HandlerError as AsRef<dyn std::error::Error>>::as_ref(&error)
                .to_string()
                .starts_with("Retryable error")
        );
    }
    #[tokio::test]
    async fn another_active_installation_for_same_app_account_conflicts() {
        let state = fixture_state().await;
        let mut change = transition();
        project_installation(&state, &change).await.unwrap();
        change
            .installation
            .as_mut()
            .unwrap()
            .account
            .installation_id = 2;
        assert!(project_installation(&state, &change).await.is_err());
    }
    #[tokio::test]
    async fn repository_refresh_and_uninstall_project_full_revisions() {
        let state = fixture_state().await;
        let mut change = transition();
        project_installation(&state, &change).await.unwrap();
        let value = change.installation.as_mut().unwrap();
        value.revision = 2;
        value.account.selected_repos = SelectedRepos::Subset(vec![10, 20]);
        project_installation(&state, &change).await.unwrap();
        assert_eq!(
            state
                .storage
                .app_installation(App::Repo, 1)
                .await
                .unwrap()
                .unwrap()
                .account
                .selected_repos,
            SelectedRepos::Subset(vec![10, 20])
        );
        let value = change.installation.as_mut().unwrap();
        value.revision = 3;
        value.account.uninstalled_at = Some(dt("2026-01-02T00:00:00Z"));
        project_installation(&state, &change).await.unwrap();
        assert!(
            state
                .storage
                .active_app_installation(App::Repo, 100)
                .await
                .unwrap()
                .is_none()
        );
        let stale = transition();
        project_installation(&state, &stale).await.unwrap();
        assert!(
            state
                .storage
                .active_app_installation(App::Repo, 100)
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn retained_installation_audit_replay_keeps_one_event() {
        let (state, storage) = fixture_state_with_storage().await;
        let change = transition();
        project_installation(&state, &change).await.unwrap();
        state
            .storage
            .audit(change.event.as_ref().unwrap())
            .await
            .unwrap();
        state
            .storage
            .audit(change.event.as_ref().unwrap())
            .await
            .unwrap();
        assert_eq!(storage.debug_list_audit(100).await.unwrap().len(), 1);
    }
    #[test]
    fn revision_cannot_exceed_portable_storage_domain() {
        assert_eq!(next_revision(1).unwrap(), 2);
        assert!(next_revision(ghinvite_core::storage::integer::MAX_EXACT).is_err());
    }
    /// GM-00 (PD-9): the `verify_onboard_identity` run journals gh-toolkit's
    /// `Option<GhAppInstallation>` with its own serde derive. A replay decodes
    /// the journaled bytes, so the shape must survive the migration.
    #[test]
    fn golden_verify_onboard_identity_journal() {
        use ghinvite_github::payloads::GhAppInstallation;
        // As `get_installation` decodes GitHub's response.
        let wire = serde_json::json!({
            "id": 77, "app_id": 123, "target_type": "Organization",
            "created_at": "2026-05-20T11:50:00Z", "updated_at": "2026-05-21T08:00:00Z",
            "suspended_at": "2026-06-01T00:00:00Z",
            "account": {"id": 9001, "login": "acme", "type": "Organization",
                "avatar_url": "https://avatars.githubusercontent.com/u/9001?v=4"},
            "repository_selection": "selected",
            "permissions": {"administration": "write", "members": "read", "metadata": "read"}
        });
        let installation: GhAppInstallation = serde_json::from_value(wire).unwrap();
        let decoded =
            crate::test_support::golden::json("verify_onboard_identity.json", &Some(installation))
                .unwrap();
        assert_eq!(decoded.account.id, 9001);
        assert_eq!(decoded.permissions.len(), 3);
        crate::test_support::golden::json::<Option<GhAppInstallation>>(
            "verify_onboard_identity_absent.json",
            &None,
        );
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WebhookExpiry {
    pub payload_sha256: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}
fn valid_payload_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
impl AccountInstallation {
    async fn webhook_time(
        &self,
        ctx: &ObjectContext<'_>,
    ) -> Result<chrono::DateTime<chrono::Utc>, TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(clock) = &self.state.installation_clock {
            let clock = clock.clone();
            return crate::clock::with_clock(ctx, "webhook_receipt_time", move || {
                chrono::DateTime::from_timestamp_millis(
                    clock.load(std::sync::atomic::Ordering::SeqCst),
                )
                .expect("fixture clock")
            })
            .await;
        }
        crate::clock::now(ctx, "webhook_receipt_time").await
    }
    async fn retain_webhook(
        &self,
        ctx: &ObjectContext<'_>,
        payload_sha256: String,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), TerminalError> {
        let expires_at = now
            .checked_add_signed(chrono::Duration::days(7))
            .ok_or_else(invalid)?;
        ctx.set(&format!("webhook/{payload_sha256}"), Json(expires_at));
        ctx.object_client::<AccountInstallationClient>(ctx.key())
            .expire_webhook(Json(WebhookExpiry {
                payload_sha256,
                expires_at,
            }))
            .send_after(std::time::Duration::from_secs(7 * 24 * 60 * 60))
            .await?;
        Ok(())
    }
}
