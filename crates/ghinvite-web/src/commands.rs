//! Concrete app-scoped Restate ingress commands (ADR0008).
use crate::{error::Result, restate_client::RestateClient};
use ghinvite_core::account::{InstallationRef, InstallationWebhookInput, OnboardInput};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RestateCommands {
    restate: Arc<RestateClient>,
}
impl RestateCommands {
    pub fn new(restate: Arc<RestateClient>) -> Self {
        Self { restate }
    }

    pub async fn onboard_installation(&self, command: OnboardInput) -> Result<()> {
        let key = installation_key(&command.verified)?;
        self.restate
            .call("Installation", &key, "onboard", &command)
            .await
            .map_err(mutation_outcome)
    }

    pub async fn organization_facts(
        &self,
        account_id: u64,
    ) -> Result<Option<ghinvite_core::org_eligibility::Facts>> {
        let key = ghinvite_core::account::AccountInstallationRef {
            app: ghinvite_core::account::App::Org,
            account_id,
        }
        .key()
        .map_err(|_| crate::WebError::BadRequest("invalid organization identity".into()))?;
        self.restate
            .call("OrganizationEligibility", &key, "facts", &())
            .await
            .map_err(mutation_outcome)
    }
    pub async fn organization_member_removed(
        &self,
        input: ghinvite_core::org_eligibility::MemberRemoved,
    ) -> Result<()> {
        input.validate().map_err(|_| {
            crate::WebError::BadRequest("invalid organization capacity hint".into())
        })?;
        let key = format!("org:{}", input.account_id);
        self.restate
            .send("OrganizationEligibility", &key, "member_removed", &input)
            .await
            .map_err(mutation_outcome)
    }

    pub async fn organization_webhook(
        &self,
        wake: ghinvite_core::org_webhook::OrganizationWebhookWake,
    ) -> Result<()> {
        wake.validate().map_err(|_| {
            crate::WebError::BadRequest("invalid organization webhook identity".into())
        })?;
        let key = ghinvite_core::delivery::delivery_key(
            wake.identity.request_id,
            wake.identity.target_id,
            wake.identity.delivery_generation,
        );
        self.restate
            .send("OrganizationDelivery", &key, "on_webhook", &wake)
            .await
            .map_err(mutation_outcome)
    }

    pub async fn installation_webhook(&self, command: InstallationWebhookInput) -> Result<()> {
        let key = installation_key(&command.verified)?;
        self.restate
            .send("Installation", &key, "onboard_webhook", &command)
            .await
            .map_err(mutation_outcome)
    }

    pub async fn set_delegation(
        &self,
        account_id: u64,
        command: ghinvite_core::account::SetDelegationInput,
    ) -> Result<std::result::Result<(), ghinvite_core::account::DelegationRefusal>> {
        let key = ghinvite_core::account::AccountInstallationRef {
            app: ghinvite_core::account::App::Repo,
            account_id,
        }
        .key()
        .map_err(|_| crate::WebError::BadRequest("invalid account identity".into()))?;
        self.restate
            .call("AccountInstallation", &key, "set_delegation", &command)
            .await
            .map_err(mutation_outcome)
    }

    pub async fn org_cap_policy(
        &self,
        account_id: u64,
    ) -> Result<ghinvite_core::budget::OrgCapView> {
        let key = ghinvite_core::account::AccountInstallationRef {
            app: ghinvite_core::account::App::Repo,
            account_id,
        }
        .key()
        .map_err(|_| crate::WebError::BadRequest("invalid account identity".into()))?;
        self.restate
            .call("AccountInstallation", &key, "org_cap_policy", &())
            .await
    }
    pub async fn set_org_cap(
        &self,
        account_id: u64,
        command: ghinvite_core::budget::SetOrgCap,
    ) -> Result<ghinvite_core::budget::OrgCapOutcome> {
        let key = ghinvite_core::account::AccountInstallationRef {
            app: ghinvite_core::account::App::Repo,
            account_id,
        }
        .key()
        .map_err(|_| crate::WebError::BadRequest("invalid account identity".into()))?;
        self.restate
            .call("AccountInstallation", &key, "set_org_cap", &command)
            .await
            .map_err(|error| match error {
                crate::WebError::Restate(crate::error::IngressFailure::Rejected {
                    status: 409,
                }) => crate::WebError::Conflict,
                other => mutation_outcome(other),
            })
    }

    pub async fn refresh_installation(&self, installation: InstallationRef) -> Result<()> {
        let key = installation
            .key()
            .map_err(|_| crate::WebError::BadRequest("invalid installation identity".into()))?;
        self.restate
            .send("Installation", &key, "recheck_permissions", &())
            .await
            .map_err(mutation_outcome)
    }
}
fn installation_key(facts: &ghinvite_core::account::InstallationFacts) -> Result<String> {
    InstallationRef {
        app: facts.app,
        installation_id: facts.account.installation_id,
    }
    .key()
    .map_err(|_| crate::WebError::BadRequest("invalid installation identity".into()))
}

// A rejected or unreachable ingress may have persisted a mutation before losing
// the acknowledgement. Never report that as proof that no effect occurred.
fn mutation_outcome(error: crate::WebError) -> crate::WebError {
    match error {
        crate::WebError::Restate(failure) => {
            crate::WebError::Restate(failure.into_outcome_unknown())
        }
        other => other,
    }
}
