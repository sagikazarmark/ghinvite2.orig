//! `Installation` Virtual Object: the installation-keyed entry point GitHub's
//! webhooks still address. It owns no installation history of its own — every
//! handler resolves the account and hands the transition to
//! [`crate::availability::AccountInstallationV1`], which is where installation
//! facts and their projections live.

use crate::state::AppState;
use chrono::{DateTime, Utc};
use ghinvite_core::{AccountType, SelectedRepos};
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
