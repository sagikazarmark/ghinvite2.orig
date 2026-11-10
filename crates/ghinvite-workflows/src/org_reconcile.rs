//! One complete pending/failed list pair per tracked organization, followed by
//! independent delivery observations. SQL projections never discover sweep work.
use crate::{AppState, reconcile::TargetInput};
use chrono::Utc;
use ghinvite_core::{
    account::App,
    org_delivery::OrganizationInvitationRole,
    org_observation::{
        FailedOrganizationInvitation, ListedOrganizationInvitation, OrganizationObservation,
    },
};
use ghinvite_github::{
    AppSlot,
    endpoints::organization as api,
    error::{ApiError, CallError},
    profiles::ORG_READ,
};
use restate_sdk::{
    context::{Context, ContextClient, ContextSideEffects, RunFuture},
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObservationBatch {
    pub target: TargetInput,
    pub observation: OrganizationObservation,
}
pub struct OrganizationReconcile {
    pub state: AppState,
}
fn listed(value: api::OrganizationInvitation) -> Result<ListedOrganizationInvitation, ApiError> {
    Ok(ListedOrganizationInvitation {
        upstream_id: value.id,
        login: value.login,
        created_at: value.created_at,
        team_count: value.team_count,
        role: match value.role {
            api::InvitationRole::DirectMember => OrganizationInvitationRole::DirectMember,
            api::InvitationRole::Admin => OrganizationInvitationRole::Admin,
            api::InvitationRole::BillingManager => OrganizationInvitationRole::BillingManager,
            api::InvitationRole::HiringManager => OrganizationInvitationRole::HiringManager,
            _ => {
                return Err(ApiError::Decode {
                    what: "organization_invitation_role",
                });
            }
        },
    })
}
#[restate_sdk::service]
impl OrganizationReconcile {
    #[handler]
    async fn target(
        &self,
        ctx: Context<'_>,
        Json(input): Json<TargetInput>,
    ) -> Result<(), TerminalError> {
        if input.account.app != App::Org || input.account.account_id != input.target_id {
            return Err(TerminalError::new_with_code(
                400,
                "organization sweep target mismatch",
            ));
        }
        let account_key = input.account.key().map_err(TerminalError::new)?;
        if input.deliveries.is_empty() {
            return Ok(());
        }
        let Json(Some(account)) = ctx
            .object_client::<crate::availability::AccountInstallationClient>(account_key)
            .current_installation()
            .call()
            .await?
        else {
            return Ok(());
        };
        let Json(observation) = ctx
            .run(|| async {
                let apps = self.state.github_apps.as_ref().ok_or_else(|| {
                    TerminalError::new_with_code(503, "organization app runtime unavailable")
                })?;
                let app = apps
                    .apps
                    .app(AppSlot::Org)
                    .map_err(|_| TerminalError::new("installed app unavailable"))?;
                let observation_started_at = Utc::now();
                let account = &account;
                let input = &input;
                let result = app
                    .read(account.installation_id, &ORG_READ, |handle| async move {
                        let rest = handle.rest();
                        let org = api::get_organization(&rest, &account.account_login).await?;
                        if org.id != input.target_id || account.account_id != input.target_id {
                            return Err(CallError::<std::convert::Infallible>::Api(
                                ApiError::Decode {
                                    what: "organization_sweep_identity",
                                },
                            ));
                        }
                        let pending = api::list_invitations(&rest, &org.login)
                            .await?
                            .into_iter()
                            .map(listed)
                            .collect::<Result<Vec<_>, _>>()?;
                        let failed = api::list_failed_invitations(&rest, &org.login)
                            .await?
                            .into_iter()
                            .map(|v| {
                                Ok::<_, ApiError>(FailedOrganizationInvitation {
                                    invitation: listed(v.invitation)?,
                                    failed_at: v.failed_at,
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let observation = OrganizationObservation {
                            account_id: account.account_id,
                            installation_id: account.installation_id,
                            org_id: org.id,
                            org_login: org.login,
                            observation_started_at,
                            observed_at: Utc::now(),
                            pending,
                            failed,
                        };
                        observation.validate().map_err(|_| {
                            CallError::Api(ApiError::Decode {
                                what: "organization_sweep_list",
                            })
                        })?;
                        Ok(observation)
                    })
                    .await;
                // Failure or a partial list supplies no evidence. Leave registrations
                // intact for the next sweep instead of spawning a second retry chain.
                let observation = match result {
                    Ok(value) => Some(value),
                    Err(_) => {
                        tracing::warn!("organization sweep observation unavailable");
                        None
                    }
                };
                Ok::<_, HandlerError>(Json(observation))
            })
            .name("list_organization_invitations")
            .await?;
        if let Some(observation) = observation {
            ctx.service_client::<OrganizationReconcileClient>()
                .observe_next(Json(ObservationBatch {
                    target: input,
                    observation,
                }))
                .send()
                .await?;
        }
        Ok(())
    }
    #[handler]
    async fn observe_next(
        &self,
        ctx: Context<'_>,
        Json(mut batch): Json<ObservationBatch>,
    ) -> Result<(), TerminalError> {
        batch.observation.validate().map_err(TerminalError::new)?;
        if batch.target.account.app != App::Org
            || batch.target.account.account_id != batch.observation.account_id
            || batch.target.target_id != batch.observation.org_id
        {
            return Err(TerminalError::new_with_code(
                400,
                "organization sweep observation mismatch",
            ));
        }
        let Some(key) = batch.target.deliveries.pop() else {
            return Ok(());
        };
        if !batch.target.deliveries.is_empty() {
            ctx.service_client::<OrganizationReconcileClient>()
                .observe_next(Json(batch.clone()))
                .send()
                .await?;
        }
        if let Some(email_key) = key.strip_prefix("email:") {
            ctx.object_client::<crate::email_delivery::EmailInviteDeliveryClient>(email_key)
                .reconcile(Json(batch.observation))
                .send()
                .await?;
            return Ok(());
        }
        ctx.object_client::<crate::org_delivery::OrganizationDeliveryClient>(key)
            .observe_listed(Json(batch.observation))
            .send()
            .await?;
        Ok(())
    }
}
