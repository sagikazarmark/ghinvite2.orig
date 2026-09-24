//! `Installation` Virtual Object: the installation-keyed entry point GitHub's
//! webhooks still address. It owns no installation history of its own — each
//! handler resolves the account and calls
//! [`crate::availability::AccountInstallation`], which is where installation
//! facts and their projections live.
//!
//! `onboard` and `uninstall` pass their input on. `repos_changed` does not: a
//! repository-change payload keeps its old wire shape for compatibility, but
//! its selection is ignored and the handler asks for a refresh instead, so the
//! stored scope comes from what GitHub currently shows rather than from what an
//! event claimed (docs/installation-availability.md).

use crate::state::AppState;
use chrono::{DateTime, Utc};
use ghinvite_core::{AccountType, SelectedRepos};
use restate_sdk::context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext};
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

pub struct Installation {
    pub state: AppState,
}

#[restate_sdk::object]
impl Installation {
    #[handler]
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
        ctx.object_client::<crate::availability::AccountInstallationClient>(
            input.account_id.to_string(),
        )
        .onboard(Json(input))
        .call()
        .await
    }

    #[handler]
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
        if let Some(account_id) = ctx.get::<u64>("account_id").await? {
            ctx.object_client::<crate::availability::AccountInstallationClient>(
                account_id.to_string(),
            )
            .refresh(Json(input.installation_id))
            .call()
            .await?;
        }
        Ok(())
    }

    #[handler]
    async fn uninstall(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<UninstallInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        validate_key(&ctx, input.installation_id)?;
        // A redelivered uninstall has nothing left to pass on: the first one
        // already reached the account, which retains its own continuation.
        if ctx.get::<bool>("uninstalled").await?.is_some() {
            return Ok(());
        }
        ctx.set("uninstalled", true);
        if let Some(account_id) = ctx.get::<u64>("account_id").await? {
            ctx.object_client::<crate::availability::AccountInstallationClient>(
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
