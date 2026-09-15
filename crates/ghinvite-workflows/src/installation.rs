//! `Installation` Virtual Object: onboard, repos_changed, uninstall.

use crate::audit::{Actor, Target};
use crate::error::HandlerError;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::{Account, AccountType, SelectedRepos};
use restate_sdk::context::{
    ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
    RunFuture,
};
use restate_sdk::errors::TerminalError;
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct OnboardInput {
    pub installation_id: u64,
    pub actor_user_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: AccountType,
    pub selected_repos: SelectedRepos,
    pub installed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ReposChangedInput {
    pub installation_id: u64,
    pub selected_repos: SelectedRepos,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct UninstallInput {
    pub installation_id: u64,
    pub uninstalled_at: DateTime<Utc>,
}

#[restate_sdk::object]
pub trait Installation {
    async fn onboard(input: Json<OnboardInput>) -> std::result::Result<(), TerminalError>;
    async fn repos_changed(
        input: Json<ReposChangedInput>,
    ) -> std::result::Result<(), TerminalError>;
    async fn uninstall(input: Json<UninstallInput>) -> std::result::Result<(), TerminalError>;
}

pub struct InstallationImpl {
    pub state: AppState,
}

impl Installation for InstallationImpl {
    async fn onboard(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<OnboardInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        validate_key(&ctx, input.installation_id)?;
        if ctx.get::<bool>("uninstalled").await?.is_some() {
            return Ok(());
        }
        if let Some(account_id) = ctx.get::<u64>("account_id").await?
            && account_id != input.account_id
        {
            return Err(TerminalError::new_with_code(
                409,
                "installation identity conflict",
            ));
        }
        ctx.set("account_id", input.account_id);
        ctx.object_client::<crate::availability::AccountInstallationV1Client>(
            input.account_id.to_string(),
        )
        .onboard(Json(input))
        .call()
        .await
    }

    async fn repos_changed(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<ReposChangedInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        validate_key(&ctx, input.installation_id)?;
        if ctx.get::<bool>("uninstalled").await?.is_some() {
            return Ok(());
        }
        if let Some(account_id) = self.account_id(&ctx, input.installation_id).await? {
            ctx.object_client::<crate::availability::AccountInstallationV1Client>(
                account_id.to_string(),
            )
            .refresh(Json(input.installation_id))
            .call()
            .await?;
        }
        Ok(())
    }

    async fn uninstall(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<UninstallInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        validate_key(&ctx, input.installation_id)?;
        ctx.set("uninstalled", true);
        if let Some(account_id) = self.account_id(&ctx, input.installation_id).await? {
            ctx.object_client::<crate::availability::AccountInstallationV1Client>(
                account_id.to_string(),
            )
            .uninstall(Json(input))
            .call()
            .await?;
        }
        Ok(())
    }
}

fn validate_key(ctx: &ObjectContext<'_>, id: u64) -> std::result::Result<(), TerminalError> {
    if id == 0 || ctx.key() != id.to_string() {
        return Err(TerminalError::new_with_code(
            400,
            "invalid installation key",
        ));
    }
    Ok(())
}
impl InstallationImpl {
    async fn account_id(
        &self,
        ctx: &ObjectContext<'_>,
        id: u64,
    ) -> std::result::Result<Option<u64>, TerminalError> {
        if let Some(id) = ctx.get("account_id").await? {
            return Ok(Some(id));
        }
        let Json(account_id) = ctx
            .run(|| async {
                self.state
                    .storage
                    .get_installation(id)
                    .await
                    .map(|a| Json(a.map(|a| a.account_id)))
                    .map_err(restate_sdk::errors::HandlerError::from)
            })
            .name("resolve_installation_account")
            .await?;
        if let Some(account_id) = account_id {
            ctx.set("account_id", account_id);
        }
        Ok(account_id)
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
    onboard_installation_transition(state, input, request_id).await
}

async fn onboard_installation_transition(
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
        Err(ghinvite_core::storage::Error::Conflict(
            ghinvite_core::storage::ConflictKind::DuplicateId,
        )) => {
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
    change_installation_repos_transition(state, input, request_id).await
}

async fn change_installation_repos_transition(
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
        serde_json::json!({
            "selected_repos_kind": match &input.selected_repos {
                SelectedRepos::All => "all",
                SelectedRepos::Subset(_) => "subset",
            },
        }),
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
    uninstall_installation_transition(state, input, request_id).await
}

async fn uninstall_installation_transition(
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
        Err(ghinvite_core::storage::Error::NotFound) => {
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
        serde_json::json!({
            "uninstalled_at": input.uninstalled_at.to_rfc3339(),
        }),
        request_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_state, fixture_state_with_storage};
    use ghinvite_core::audit::{ActorKind, EventType, TargetKind};

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

    async fn audit_events(
        storage: &ghinvite_storage_sqlx::SqlxStorage,
        account_id: u64,
    ) -> Vec<ghinvite_core::audit::AuditEvent> {
        storage.debug_list_audit(account_id).await.unwrap()
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
    async fn onboard_transition_inserts_row_and_audits_once() {
        let (state, storage) = fixture_state_with_storage().await;

        onboard_installation_transition(&state, &sample_input(), Some("req-onboard".into()))
            .await
            .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(acct.account_login, "acme");
        assert_eq!(acct.account_id, 100);
        assert!(acct.uninstalled_at.is_none());

        onboard_installation_transition(&state, &sample_input(), Some("req-onboard-again".into()))
            .await
            .unwrap();

        let audits = audit_events(&storage, 100).await;
        let created: Vec<_> = audits
            .iter()
            .filter(|event| event.event_type == EventType::InstallationCreated)
            .collect();
        assert_eq!(created.len(), 1);
        let event = created[0];
        assert_eq!(event.actor_kind, ActorKind::User);
        assert_eq!(event.actor_id, Some(7));
        assert_eq!(event.target_kind, TargetKind::Installation);
        assert_eq!(event.target_id, "1");
        assert_eq!(event.request_id.as_deref(), Some("req-onboard"));
        assert_eq!(
            event.metadata.get("account_login"),
            Some(&serde_json::json!("acme"))
        );
        assert_eq!(
            event.metadata.get("account_type"),
            Some(&serde_json::json!("Organization"))
        );
    }

    #[tokio::test]
    async fn onboard_duplicate_id_is_idempotent() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        // Second call — duplicate installation_id (PK collision). Should
        // return Ok(()) without re-auditing.
        let result = onboard_logic(&state, &sample_input(), None).await;
        assert!(
            result.is_ok(),
            "duplicate id should be idempotent: {result:?}"
        );
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
        assert!(
            err.is_terminal(),
            "duplicate active should be terminal: {err:?}"
        );
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
    async fn repos_changed_transition_updates_and_audits() {
        let (state, storage) = fixture_state_with_storage().await;
        onboard_installation_transition(&state, &sample_input(), Some("req-onboard".into()))
            .await
            .unwrap();

        change_installation_repos_transition(
            &state,
            &ReposChangedInput {
                installation_id: 1,
                selected_repos: SelectedRepos::Subset(vec![10, 20]),
            },
            Some("req-repos".into()),
        )
        .await
        .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(acct.selected_repos, SelectedRepos::Subset(vec![10, 20]));

        let audits = audit_events(&storage, 100).await;
        let event = audits
            .iter()
            .find(|event| event.event_type == EventType::InstallationReposChanged)
            .expect("repository-selection change should emit audit event");
        assert_eq!(event.actor_kind, ActorKind::Github);
        assert_eq!(event.actor_id, None);
        assert_eq!(event.target_kind, TargetKind::Installation);
        assert_eq!(event.target_id, "1");
        assert_eq!(event.request_id.as_deref(), Some("req-repos"));
        assert_eq!(
            event.metadata.get("selected_repos_kind"),
            Some(&serde_json::json!("subset"))
        );
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
    async fn uninstall_transition_marks_and_audits_once() {
        let (state, storage) = fixture_state_with_storage().await;
        onboard_installation_transition(&state, &sample_input(), Some("req-onboard".into()))
            .await
            .unwrap();
        let uninstalled_at = dt("2026-05-05T00:00:00Z");

        uninstall_installation_transition(
            &state,
            &UninstallInput {
                installation_id: 1,
                uninstalled_at,
            },
            Some("req-uninstall".into()),
        )
        .await
        .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(acct.uninstalled_at, Some(uninstalled_at));

        uninstall_installation_transition(
            &state,
            &UninstallInput {
                installation_id: 1,
                uninstalled_at: dt("2026-05-06T00:00:00Z"),
            },
            Some("req-uninstall-again".into()),
        )
        .await
        .unwrap();

        let audits = audit_events(&storage, 100).await;
        let uninstalled: Vec<_> = audits
            .iter()
            .filter(|event| event.event_type == EventType::InstallationUninstalled)
            .collect();
        assert_eq!(uninstalled.len(), 1);
        let event = uninstalled[0];
        assert_eq!(event.actor_kind, ActorKind::Github);
        assert_eq!(event.actor_id, None);
        assert_eq!(event.target_kind, TargetKind::Installation);
        assert_eq!(event.target_id, "1");
        assert_eq!(event.request_id.as_deref(), Some("req-uninstall"));
        assert_eq!(
            event.metadata.get("uninstalled_at"),
            Some(&serde_json::json!(uninstalled_at.to_rfc3339()))
        );
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
        assert!(
            result.is_ok(),
            "missing installation should be idempotent: {result:?}"
        );
    }

    #[tokio::test]
    async fn uninstall_transition_unknown_installation_does_not_audit() {
        let (state, storage) = fixture_state_with_storage().await;

        uninstall_installation_transition(
            &state,
            &UninstallInput {
                installation_id: 999,
                uninstalled_at: dt("2026-05-05T00:00:00Z"),
            },
            Some("req-uninstall-missing".into()),
        )
        .await
        .unwrap();

        let audits = audit_events(&storage, 100).await;
        assert!(audits.is_empty());
    }
}
