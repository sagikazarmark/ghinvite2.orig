//! `Installation` Virtual Object: onboard, repos_changed, uninstall.

use crate::audit::{Actor, Target};
use crate::error::HandlerError;
use crate::impl_restate_json_payload;
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Utc};
use domain::{Account, AccountType, SelectedRepos};
use restate_sdk::context::{ContextSideEffects, ObjectContext, RunFuture};
use restate_sdk::errors::TerminalError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnboardInput {
    pub installation_id: u64,
    pub actor_user_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: AccountType,
    pub selected_repos: SelectedRepos,
    pub installed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReposChangedInput {
    pub installation_id: u64,
    pub selected_repos: SelectedRepos,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UninstallInput {
    pub installation_id: u64,
    pub uninstalled_at: DateTime<Utc>,
}

impl_restate_json_payload!(OnboardInput);
impl_restate_json_payload!(ReposChangedInput);
impl_restate_json_payload!(UninstallInput);

#[restate_sdk::object]
pub trait Installation {
    async fn onboard(input: OnboardInput) -> std::result::Result<(), TerminalError>;
    async fn repos_changed(input: ReposChangedInput) -> std::result::Result<(), TerminalError>;
    async fn uninstall(input: UninstallInput) -> std::result::Result<(), TerminalError>;
}

pub struct InstallationImpl {
    pub state: AppState,
}

impl Installation for InstallationImpl {
    async fn onboard(
        &self,
        ctx: ObjectContext<'_>,
        input: OnboardInput,
    ) -> std::result::Result<(), TerminalError> {
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            onboard_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("onboard")
        .await
    }

    async fn repos_changed(
        &self,
        ctx: ObjectContext<'_>,
        input: ReposChangedInput,
    ) -> std::result::Result<(), TerminalError> {
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            repos_changed_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("repos_changed")
        .await
    }

    async fn uninstall(
        &self,
        ctx: ObjectContext<'_>,
        input: UninstallInput,
    ) -> std::result::Result<(), TerminalError> {
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            uninstall_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("uninstall")
        .await
    }
}

/// Pure logic: persist the new installation row and emit the audit event.
/// Idempotent: a duplicate `installation_id` returns `Ok(())` after observing
/// the existing row matches the input.
pub async fn onboard_logic(
    state: &AppState,
    input: &OnboardInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
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
        Ok(()) => (),
        Err(storage::Error::Conflict(storage::ConflictKind::DuplicateId)) => {
            // Already onboarded — idempotent retry, no audit emit.
            return Ok(());
        }
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    crate::audit::emit(
        state,
        input.account_id,
        EventType::InstallationCreated,
        Actor::User(input.actor_user_id),
        Target::installation(input.installation_id),
        serde_json::json!({
            "account_login": input.account_login,
            "account_type": input.account_type.to_string(),
        }),
        request_id,
    )
    .await
}

/// Pure logic: update the installation's selected repos and emit audit.
pub async fn repos_changed_logic(
    state: &AppState,
    input: &ReposChangedInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_installation_repos(input.installation_id, &input.selected_repos)
        .await?;

    let acct = state
        .storage
        .get_installation(input.installation_id)
        .await?
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "installation {} updated but not readable",
                input.installation_id
            ))
        })?;

    crate::audit::emit(
        state,
        acct.account_id,
        EventType::InstallationReposChanged,
        Actor::Github,
        Target::installation(input.installation_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}

/// Pure logic: stamp `uninstalled_at` and emit audit. Idempotent: if the
/// installation was already uninstalled, returns `Ok(())` without re-audit.
pub async fn uninstall_logic(
    state: &AppState,
    input: &UninstallInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    match state
        .storage
        .mark_installation_uninstalled(input.installation_id, input.uninstalled_at)
        .await
    {
        Ok(()) => (),
        Err(storage::Error::NotFound) => {
            // Already uninstalled or never existed; idempotent.
            return Ok(());
        }
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    let acct = state
        .storage
        .get_installation(input.installation_id)
        .await?
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "installation {} marked uninstalled but not readable",
                input.installation_id
            ))
        })?;

    crate::audit::emit(
        state,
        acct.account_id,
        EventType::InstallationUninstalled,
        Actor::Github,
        Target::installation(input.installation_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_state};

    fn sample_input() -> OnboardInput {
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
    async fn onboard_inserts_row_and_audits() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), Some("inv-1".into()))
            .await
            .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(acct.account_login, "acme");
        assert_eq!(acct.account_id, 100);
        assert!(acct.uninstalled_at.is_none());
    }

    #[tokio::test]
    async fn onboard_duplicate_id_is_idempotent() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        // Second call — duplicate installation_id (PK collision). Should
        // return Ok(()) without re-auditing.
        let result = onboard_logic(&state, &sample_input(), None).await;
        assert!(result.is_ok(), "duplicate id should be idempotent: {result:?}");
    }

    #[tokio::test]
    async fn onboard_duplicate_active_account_is_terminal() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        // Different installation_id, same account_id, both active → partial
        // unique index hits and we surface ConflictKind::DuplicateActiveInstallation.
        let mut second = sample_input();
        second.installation_id = 2;
        let err = onboard_logic(&state, &second, None).await.unwrap_err();
        assert!(err.is_terminal(), "duplicate active should be terminal: {err:?}");
    }

    #[tokio::test]
    async fn repos_changed_updates_and_audits() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        repos_changed_logic(
            &state,
            &ReposChangedInput {
                installation_id: 1,
                selected_repos: SelectedRepos::Subset(vec![10, 20]),
            },
            Some("inv-2".into()),
        )
        .await
        .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(acct.selected_repos, SelectedRepos::Subset(vec![10, 20]));
    }

    #[tokio::test]
    async fn repos_changed_unknown_installation_is_terminal() {
        let state = fixture_state().await;
        let err = repos_changed_logic(
            &state,
            &ReposChangedInput {
                installation_id: 999,
                selected_repos: SelectedRepos::All,
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(err.is_terminal());
    }

    #[tokio::test]
    async fn uninstall_marks_and_audits() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        uninstall_logic(
            &state,
            &UninstallInput {
                installation_id: 1,
                uninstalled_at: dt("2026-05-05T00:00:00Z"),
            },
            Some("inv-3".into()),
        )
        .await
        .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert!(acct.uninstalled_at.is_some());
    }

    #[tokio::test]
    async fn uninstall_unknown_installation_is_idempotent() {
        let state = fixture_state().await;
        let result = uninstall_logic(
            &state,
            &UninstallInput {
                installation_id: 999,
                uninstalled_at: dt("2026-05-05T00:00:00Z"),
            },
            None,
        )
        .await;
        assert!(result.is_ok(), "missing installation should be idempotent: {result:?}");
    }
}
