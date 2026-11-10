//! App-scoped installation entry point. Setup never records an installer;
//! only the verified signed webhook command can establish that identity.
use crate::state::AppState;
use chrono::{DateTime, Utc};
use ghinvite_core::SelectedRepos;
use ghinvite_core::account::{AccountInstallationRef, App, InstallationRef};
pub use ghinvite_core::account::{InstallationWebhookInput, OnboardInput};
use restate_sdk::context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext};
use restate_sdk::errors::TerminalError;
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ReposChangedInput {
    pub app: App,
    pub installation_id: u64,
    pub selected_repos: SelectedRepos,
}
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct UninstallInput {
    pub app: App,
    pub installation_id: u64,
    pub uninstalled_at: DateTime<Utc>,
}
pub struct Installation {
    pub state: AppState,
}

fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid installation key")
}
fn validate_key(ctx: &ObjectContext<'_>, app: App, id: u64) -> Result<(), TerminalError> {
    if ctx.key()
        != (InstallationRef {
            app,
            installation_id: id,
        })
        .key()
        .map_err(|_| invalid())?
    {
        return Err(invalid());
    }
    Ok(())
}
fn account_key(app: App, id: u64) -> Result<String, TerminalError> {
    (AccountInstallationRef {
        app,
        account_id: id,
    })
    .key()
    .map_err(|_| invalid())
}
#[restate_sdk::object]
impl Installation {
    #[handler]
    async fn onboard(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<OnboardInput>,
    ) -> Result<(), TerminalError> {
        let facts = &input.verified;
        validate_key(&ctx, facts.app, facts.account.installation_id)?;
        if ctx.get::<bool>("uninstalled").await?.is_some() {
            return Ok(());
        }
        if ctx
            .get::<u64>("account_id")
            .await?
            .is_some_and(|id| id != facts.account.account_id)
        {
            return Err(TerminalError::new_with_code(
                409,
                "installation identity conflict",
            ));
        }
        ctx.set("account_id", facts.account.account_id);
        ctx.object_client::<crate::availability::AccountInstallationClient>(account_key(
            facts.app,
            facts.account.account_id,
        )?)
        .onboard(Json(input))
        .call()
        .await
    }
    #[handler]
    async fn onboard_webhook(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InstallationWebhookInput>,
    ) -> Result<(), TerminalError> {
        let facts = &input.verified;
        validate_key(&ctx, facts.app, facts.account.installation_id)?;
        if ctx
            .get::<u64>("account_id")
            .await?
            .is_some_and(|id| id != facts.account.account_id)
        {
            return Err(TerminalError::new_with_code(
                409,
                "installation identity conflict",
            ));
        }
        ctx.set("account_id", facts.account.account_id);
        let eligibility_hint = (facts.app == App::Org
            && self.state.organization_delivery.is_some())
        .then_some(ghinvite_core::org_eligibility::InstallationHint {
            account_id: facts.account.account_id,
            installation_id: facts.account.installation_id,
        });
        let deleted = matches!(
            input.action,
            ghinvite_core::account::InstallationWebhookAction::Deleted
        );
        let outcome = ctx
            .object_client::<crate::availability::AccountInstallationClient>(account_key(
                facts.app,
                facts.account.account_id,
            )?)
            .onboard_webhook(Json(input))
            .call()
            .await?;
        // A deletion GitHub did not confirm leaves the installation live (PD-8).
        if deleted && outcome.is_none() {
            ctx.set("uninstalled", true);
        }
        if let Some(hint) = eligibility_hint {
            ctx.object_client::<crate::org_eligibility::OrganizationEligibilityClient>(format!(
                "org:{}",
                hint.account_id
            ))
            .refresh(Json(hint))
            .send()
            .await?;
        }
        Ok(())
    }
    #[handler]
    async fn recheck_permissions(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        let key: InstallationRef = ctx.key().parse().map_err(|_| invalid())?;
        if let Some(account_id) = ctx.get::<u64>("account_id").await? {
            ctx.object_client::<crate::availability::AccountInstallationClient>(account_key(
                key.app, account_id,
            )?)
            .recheck_permissions(Json(key.installation_id))
            .call()
            .await?;
        }
        Ok(())
    }
    #[handler]
    async fn repos_changed(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ReposChangedInput>,
    ) -> Result<(), TerminalError> {
        validate_key(&ctx, input.app, input.installation_id)?;
        if ctx.get::<bool>("uninstalled").await?.is_some() {
            return Ok(());
        }
        if let Some(account_id) = ctx.get::<u64>("account_id").await? {
            ctx.object_client::<crate::availability::AccountInstallationClient>(account_key(
                input.app, account_id,
            )?)
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
        Json(input): Json<UninstallInput>,
    ) -> Result<(), TerminalError> {
        validate_key(&ctx, input.app, input.installation_id)?;
        if ctx.get::<bool>("uninstalled").await?.is_some() {
            return Ok(());
        }
        if let Some(account_id) = ctx.get::<u64>("account_id").await? {
            ctx.object_client::<crate::availability::AccountInstallationClient>(account_key(
                input.app, account_id,
            )?)
            .uninstall(Json(input))
            .call()
            .await?;
        }
        ctx.set("uninstalled", true);
        Ok(())
    }
}
