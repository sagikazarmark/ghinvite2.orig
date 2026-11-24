//! Complete, numeric-identity-bound observations for the settlement reducer.
//! These functions perform reads only; the delivery owner alone changes state.
use crate::settlement_policy::{AcceptancePolicy, Evidence, Observation};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    Account, RepositoryIdentity, SelectedRepos,
    delivery::{CreateOutcome, DeliverySnapshot},
    targets::EffectiveRole,
};
use ghinvite_github::{
    InstallationRest,
    credential::QuotaBucket,
    endpoints::{self, Repository},
    error::ApiError,
    rest::Rest,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ListedInvitation {
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    pub id: u64,
    pub invitee_id: Option<u64>,
    pub permission: EffectiveRole,
    pub expired: bool,
}
pub async fn list(installation: &Rest<'_>, name: &str) -> Result<Vec<ListedInvitation>, ApiError> {
    list_within(installation, name, usize::MAX).await
}

/// [`list`] that fails instead of reading more than `max_pages` pages.
pub async fn list_within(
    installation: &Rest<'_>,
    name: &str,
    max_pages: usize,
) -> Result<Vec<ListedInvitation>, ApiError> {
    let identity = RepositoryIdentity::parse(name.to_owned()).map_err(|_| ApiError::Decode {
        what: "repository_identity",
    })?;
    endpoints::list_repo_invitations_within(
        installation,
        Repository {
            owner: identity.owner(),
            name: identity.name(),
        },
        max_pages,
    )
    .await?
    .into_iter()
    .map(|v| {
        Ok(ListedInvitation {
            id: v.id,
            created_at: Some(v.created_at),
            invitee_id: v.invitee.map(|u| u.id),
            permission: crate::repository_protocol::observed_role(&v.permissions)?,
            expired: v.expired,
        })
    })
    .collect()
}

pub async fn observe(
    installation: &InstallationRest,
    app: &Rest<'_>,
    account: &Account,
    snapshot: &DeliverySnapshot,
    at: DateTime<Utc>,
    policy: AcceptancePolicy,
) -> Result<Option<Observation>, ApiError> {
    if snapshot.settlement.is_some()
        || snapshot.tracking_stopped.is_some()
        || !(snapshot.create.outcome.needs_settlement()
            || matches!(snapshot.create.outcome, CreateOutcome::OutcomeUnknown))
        || account.account_id != snapshot.create.command.account_id
        || account.uninstalled_at.is_some()
        || matches!(&account.selected_repos, SelectedRepos::Subset(ids) if !ids.contains(&snapshot.create.command.repo_id))
    {
        return Ok(None);
    }
    let invitations = list(
        &installation.rest(),
        &snapshot.create.command.repo_full_name,
    )
    .await?;
    observe_listed(
        installation,
        app,
        account,
        snapshot,
        at,
        policy,
        &invitations,
    )
    .await
}

/// `installation` is a REPO_READ token for the account's current
/// installation. `app` is the same app's opaque JWT credential. The caller
/// journals the resulting observation, including its supplied clock value.
pub async fn observe_listed(
    installation: &InstallationRest,
    app: &Rest<'_>,
    account: &Account,
    snapshot: &DeliverySnapshot,
    at: DateTime<Utc>,
    policy: AcceptancePolicy,
    invitations: &[ListedInvitation],
) -> Result<Option<Observation>, ApiError> {
    let command = &snapshot.create.command;
    if snapshot.tracking_stopped.is_some()
        || snapshot.settlement.is_some()
        || account.account_id != command.account_id
        || account.uninstalled_at.is_some()
        || matches!(&account.selected_repos, SelectedRepos::Subset(ids) if !ids.contains(&command.repo_id))
    {
        return Ok(None);
    }
    let upstream_id = match snapshot.create.outcome {
        CreateOutcome::Created { upstream_id }
        | CreateOutcome::AlreadyPending { upstream_id, .. } => Some(upstream_id),
        CreateOutcome::OutcomeUnknown => None,
        _ => return Ok(None),
    };
    let credential = installation.credential();
    if !matches!(app.bucket(), QuotaBucket::App(jwt_app)
        if credential.installation_id().get() == account.installation_id
            && credential.app_id() == jwt_app)
    {
        return Err(ApiError::Decode {
            what: "observation_credential_binding",
        });
    }
    let installation = &installation.rest();
    let identity = RepositoryIdentity::parse(command.repo_full_name.clone()).map_err(|_| {
        ApiError::Decode {
            what: "repository_identity",
        }
    })?;
    let repository = Repository {
        owner: identity.owner(),
        name: identity.name(),
    };
    // list_repo_invitations only returns after all pages complete. An error,
    // invalid next link or partial SSO response cannot become an empty listing.
    let found = match upstream_id {
        Some(id) => invitations.iter().find(|invitation| invitation.id == id),
        None => {
            let mut matching = invitations
                .iter()
                .filter(|invitation| invitation.invitee_id == Some(command.requester_id));
            let first = matching.next();
            if matching.next().is_some() {
                return Ok(None);
            }
            first
        }
    };
    let evidence = if let Some(invitation) = found {
        if invitation.id == 0 {
            return Ok(None);
        }
        match upstream_id {
            Some(_) => Evidence::Listed {
                upstream_id: invitation.id,
                expired: invitation.expired,
                created_at: invitation.created_at,
            },
            None => Evidence::FoundByInvitee {
                upstream_id: invitation.id,
                permission: invitation.permission.clone(),
            },
        }
    } else {
        // Positive presence is the ONLY resolution of uncertain creation.
        if upstream_id.is_none() {
            return Ok(None);
        }
        let user = endpoints::get_user_by_id(installation, command.requester_id).await?;
        if user.id != command.requester_id {
            return Ok(None);
        }
        let bound = endpoints::get_user_by_login(installation, &user.login).await?;
        if bound.id != command.requester_id || bound.login != user.login {
            return Ok(None);
        }
        let role =
            match endpoints::collaborator_permission(installation, repository, &user.login).await {
                Ok(permission) if permission.user.id == command.requester_id => {
                    crate::repository_protocol::observed_role(&permission.role)?
                }
                Ok(_) => return Ok(None),
                Err(ApiError::NotFound) => {
                    // A renamed/recycled login cannot supply negative identity evidence.
                    let rebound = endpoints::get_user_by_login(installation, &user.login).await?;
                    if rebound.id != command.requester_id || rebound.login != user.login {
                        return Ok(None);
                    }
                    EffectiveRole::None
                }
                Err(error) => return Err(error),
            };
        let direct_collaborator = if policy.require_direct_collaborator {
            Some(
                endpoints::list_collaborators(installation, repository)
                    .await?
                    .iter()
                    .any(|collaborator| collaborator.user.id == command.requester_id),
            )
        } else {
            None
        };
        Evidence::Absent {
            availability_verified: true,
            role_name: Some(role),
            direct_collaborator,
            membership: None,
        }
    };
    // Verify AFTER the complete listing and identity reads, in this observation.
    // In particular, a permission 404 caused by losing repository access must
    // not masquerade as a verified absence and settle a delivery as Declined.
    let current = endpoints::get_installation(app, account.installation_id).await?;
    if current.id != account.installation_id
        || current.account.id != command.account_id
        || current.suspended_at.is_some()
    {
        return Ok(None);
    }
    let current_repo = endpoints::get_repository(installation, repository).await?;
    if current_repo.id != command.repo_id {
        return Ok(None);
    }
    let mut observation = Observation::new(snapshot, at, evidence);
    observation.invitation_timing = found.and_then(|i| {
        i.created_at.and_then(|created_at| {
            ghinvite_core::delivery::InvitationTiming::observed(
                i.id,
                created_at,
                Utc::now(),
                command.approved_at,
                !matches!(
                    snapshot.create.outcome,
                    CreateOutcome::AlreadyPending { .. }
                ),
            )
        })
    });
    Ok(Some(observation))
}

#[cfg(test)]
mod tests;
