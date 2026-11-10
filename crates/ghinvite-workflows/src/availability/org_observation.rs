//! Bounded, read-only organization evidence. No organization delivery writes.
use super::{AccountInstallation, Observation};
use ghinvite_core::{
    Account, AccountType,
    account::{App, Installation},
};
use ghinvite_github::{
    TokenCallError,
    endpoints::{self, organization},
    error::{ApiError, CallError},
    profiles::ORG_READ,
    roles::{MembershipState, OrgRole},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

/// Absent is established only by the retained org owner, never inferred from
/// an HTTP failure. NonOwner permits the caller to consider explicit delegation;
/// pending membership, identity mismatch or inactive installation never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrgAuthority {
    Absent,
    Owner,
    NonOwner,
    Denied,
    Unavailable,
}

impl AccountInstallation {
    pub(super) async fn observe_org(&self, account: &Account) -> Observation {
        if account.account_type != AccountType::Organization || account.uninstalled_at.is_some() {
            return Observation::Unavailable;
        }
        let Ok(app) = self.github_app(App::Org) else {
            return Observation::Unknown;
        };
        match app
            .read(account.installation_id, &ORG_READ, |handle| async move {
                organization::get_organization(&handle.rest(), &account.account_login)
                    .await
                    .map_err(CallError::<Infallible>::Api)
            })
            .await
        {
            Ok(org) if org.id == account.account_id => Observation::Available { repo_ids: vec![] },
            Ok(_) => Observation::Unavailable,
            Err(TokenCallError::Call(CallError::Api(ApiError::NotFound))) => {
                Observation::Unavailable
            }
            Err(_) => Observation::Unknown,
        }
    }

    pub(super) async fn org_owner_evidence(
        &self,
        installation: &Installation,
        user_id: u64,
    ) -> OrgAuthority {
        let account = &installation.account;
        if installation.app != App::Org
            || account.account_type != AccountType::Organization
            || account.uninstalled_at.is_some()
            || installation.suspended_at.is_some()
            || user_id == 0
            || user_id > ghinvite_core::storage::integer::MAX_EXACT
        {
            return OrgAuthority::Denied;
        }
        match self
            .github_installation(App::Org, account.installation_id)
            .await
        {
            Ok(Some(value))
                if value.account.id == account.account_id
                    && value.account.account_type.as_deref() == Some("Organization")
                    && value.suspended_at.is_none()
                    && value.permissions.contains_key("members") => {}
            Ok(_) => return OrgAuthority::Denied,
            Err(_) => return OrgAuthority::Unavailable,
        }
        let Ok(app) = self.github_app(App::Org) else {
            return OrgAuthority::Unavailable;
        };
        let result = app
            .read(account.installation_id, &ORG_READ, |handle| async move {
                let rest = handle.rest();
                let org = organization::get_organization(&rest, &account.account_login)
                    .await
                    .map_err(CallError::<Infallible>::Api)?;
                if org.id != account.account_id {
                    return Ok(OrgAuthority::Denied);
                }
                let user = endpoints::get_user_by_id(&rest, user_id)
                    .await
                    .map_err(CallError::<Infallible>::Api)?;
                if user.id != user_id {
                    return Ok(OrgAuthority::Denied);
                }
                let membership =
                    match organization::organization_membership(&rest, &org.login, &user.login)
                        .await
                    {
                        Ok(value) => value,
                        Err(ApiError::NotFound) => return Ok(OrgAuthority::NonOwner),
                        Err(error) => return Err(CallError::Api(error)),
                    };
                if membership.user.id != user_id || membership.organization.id != account.account_id
                {
                    return Ok(OrgAuthority::Denied);
                }
                Ok(match (membership.state, membership.role) {
                    (MembershipState::Active, OrgRole::Admin) => OrgAuthority::Owner,
                    (MembershipState::Active, OrgRole::Member) => OrgAuthority::NonOwner,
                    _ => OrgAuthority::Denied,
                })
            })
            .await;
        result.unwrap_or(OrgAuthority::Unavailable)
    }
}

#[cfg(test)]
#[path = "org_observation_tests.rs"]
mod tests;
