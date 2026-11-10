//! Numeric identity-bound removal reads and one independently fenced DELETE.
use crate::RepositoryGithub;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    Account, SelectedRepos, access_grant::*, budget::RecordLimit,
    storage::access_grants::AccessGrantStorage,
};
use ghinvite_github::{
    AppSlot, GithubApp, TokenCallError,
    endpoints::{self, Repository, organization as org},
    error::{ApiError, CallError},
    profiles,
    roles::MembershipState,
};
use octotoken::TokenScope;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Observation {
    Ready { login: String },
    Absent,
    Skip { role: CurrentRole },
    Blocked { reason: RemovalBlocked },
    Limited { limit: RecordLimit },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteOutcome {
    Removed,
    Skipped { reason: RemovalSkip },
    NoEffect { reason: RemovalBlocked },
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    pub outcome: WriteOutcome,
    pub rejected_generation: Option<u64>,
    pub limit: Option<RecordLimit>,
}
fn blocked() -> Observation {
    Observation::Blocked {
        reason: RemovalBlocked::TargetUnavailable,
    }
}
fn read_scope(command: &AccessGrantCommand) -> &'static TokenScope {
    match command.target.app() {
        ghinvite_core::account::App::Repo => &profiles::REPO_READ,
        _ => &profiles::ORG_READ,
    }
}
fn write_scope(command: &AccessGrantCommand) -> &'static TokenScope {
    match command.target.app() {
        ghinvite_core::account::App::Repo => &profiles::REPO_INVITE,
        _ => &profiles::ORG_INVITE,
    }
}
/// The installed app that owns `command`'s target, if configured.
fn target_app(apps: &RepositoryGithub, command: &AccessGrantCommand) -> Option<GithubApp> {
    let slot = match command.target.app() {
        ghinvite_core::account::App::Repo => AppSlot::Repo,
        ghinvite_core::account::App::Org => AppSlot::Org,
    };
    apps.apps.app(slot).ok()
}
pub async fn observe(
    apps: &RepositoryGithub,
    command: &AccessGrantCommand,
    account: &Account,
    now: DateTime<Utc>,
) -> Observation {
    match observe_inner(apps, command, account).await {
        Ok(value) => value,
        Err(error) => match error.rate_limit() {
            Some(limit) => Observation::Limited {
                limit: crate::cancellation_protocol::content_limit(now, limit.retry_after),
            },
            None => Observation::Blocked {
                reason: RemovalBlocked::ReadUnavailable,
            },
        },
    }
}
async fn observe_inner(
    apps: &RepositoryGithub,
    command: &AccessGrantCommand,
    account: &Account,
) -> crate::Result<Observation> {
    if command.validate().is_err()
        || account.account_id != command.account_id
        || account.uninstalled_at.is_some()
    {
        return Ok(blocked());
    }
    if let GrantTarget::Repository { repo_id, .. } = command.target
        && matches!(&account.selected_repos,SelectedRepos::Subset(ids) if !ids.contains(&repo_id))
    {
        return Ok(blocked());
    }
    let app = target_app(apps, command)
        .ok_or_else(|| crate::HandlerError::Invariant("removal app unavailable".into()))?;
    let app_rest = app.app_rest().map_err(octotoken::TokenError::from)?;
    let installation =
        endpoints::get_installation(&app_rest.rest(), account.installation_id).await?;
    if installation.id != account.installation_id
        || installation.account.id != command.account_id
        || installation.suspended_at.is_some()
        || app.id().get() != installation.app_id
    {
        return Ok(blocked());
    }
    // Reads only: a 401 replaces the token and repeats them once.
    app.read(
        account.installation_id,
        read_scope(command),
        |handle| async move {
            let rest = handle.rest();
            let user = endpoints::get_user_by_id(&rest, command.requester_id).await?;
            if user.id != command.requester_id {
                return Ok(Observation::Blocked {
                    reason: RemovalBlocked::RequesterIdentity,
                });
            }
            let role = match &command.target {
                GrantTarget::Repository {
                    repo_id, full_name, ..
                } => {
                    let identity = ghinvite_core::RepositoryIdentity::parse(full_name.clone())
                        .map_err(|_| {
                            CallError::Api(ApiError::Decode {
                                what: "removal_repository",
                            })
                        })?;
                    let repository = Repository {
                        owner: identity.owner(),
                        name: identity.name(),
                    };
                    let current =
                        endpoints::authority::repository_identity(&rest, repository).await?;
                    if current.id != *repo_id || current.owner.id != command.account_id {
                        return Ok(blocked());
                    }
                    let permission =
                        match endpoints::collaborator_permission(&rest, repository, &user.login)
                            .await
                        {
                            Ok(value) => value,
                            Err(ApiError::NotFound) => return Ok(Observation::Absent),
                            Err(error) => return Err(CallError::Api(error)),
                        };
                    if permission.user.id != command.requester_id {
                        return Ok(blocked());
                    }
                    let role = crate::repository_protocol::observed_role(&permission.role)
                        .map_err(|_| {
                            CallError::Api(ApiError::Decode {
                                what: "removal_role",
                            })
                        })?;
                    if role == ghinvite_core::targets::EffectiveRole::None {
                        return Ok(Observation::Absent);
                    }
                    CurrentRole::Repository { role }
                }
                GrantTarget::Organization { org_id, login } => {
                    let current = org::get_organization(&rest, login).await?;
                    if current.id != *org_id {
                        return Ok(blocked());
                    }
                    let membership = match org::organization_membership(
                        &rest,
                        &current.login,
                        &user.login,
                    )
                    .await
                    {
                        Ok(value) => value,
                        Err(ApiError::NotFound) => return Ok(Observation::Absent),
                        Err(error) => return Err(CallError::Api(error)),
                    };
                    if membership.user.id != command.requester_id
                        || membership.organization.id != *org_id
                        || membership.state != MembershipState::Active
                    {
                        return Ok(blocked());
                    }
                    CurrentRole::Organization {
                        role: match membership.role {
                            ghinvite_github::roles::OrgRole::Admin => {
                                ghinvite_core::org_delivery::OrganizationRole::Admin
                            }
                            ghinvite_github::roles::OrgRole::Member => {
                                ghinvite_core::org_delivery::OrganizationRole::Member
                            }
                            _ => return Ok(blocked()),
                        },
                    }
                }
                GrantTarget::Team {
                    org_id,
                    org_login,
                    team_id,
                    slug,
                } => {
                    let current = org::get_organization(&rest, org_login).await?;
                    if current.id != *org_id {
                        return Ok(blocked());
                    }
                    let team = org::get_team(&rest, &current.login, slug).await?;
                    if team.id != *team_id || team.externally_managed {
                        return Ok(blocked());
                    }
                    let membership =
                        match org::team_membership(&rest, &current.login, &team.slug, &user.login)
                            .await
                        {
                            Ok(value) => value,
                            Err(ApiError::NotFound) => return Ok(Observation::Absent),
                            Err(error) => return Err(CallError::Api(error)),
                        };
                    if membership.state != MembershipState::Active {
                        return Ok(blocked());
                    }
                    CurrentRole::Team {
                        role: match membership.role {
                            ghinvite_github::roles::TeamRole::Maintainer => {
                                ghinvite_core::org_delivery::TeamRole::Maintainer
                            }
                            ghinvite_github::roles::TeamRole::Member => {
                                ghinvite_core::org_delivery::TeamRole::Member
                            }
                            _ => return Ok(blocked()),
                        },
                    }
                }
            };
            Ok::<_, CallError<Infallible>>(match removal_role(&command.target, &role) {
                Ok(RoleDecision::Remove) => Observation::Ready { login: user.login },
                Ok(RoleDecision::Skip) => Observation::Skip { role },
                Err(_) => blocked(),
            })
        },
    )
    .await
    .map_err(Into::into)
}
pub async fn write(
    apps: &RepositoryGithub,
    storage: &dyn AccessGrantStorage,
    input: &RemovalCommand,
    account: &Account,
    now: DateTime<Utc>,
) -> WriteResult {
    let mut result = WriteResult {
        outcome: WriteOutcome::Unknown,
        rejected_generation: None,
        limit: None,
    };
    // Existing fence is checked before all role/absence reads: a restarted run
    // must not classify an already performed DELETE as an unused reservation.
    if storage.removal_attempt_exists(input).await.unwrap_or(true) {
        return result;
    }
    let Some(app) = target_app(apps, &input.grant) else {
        result.outcome = WriteOutcome::NoEffect {
            reason: RemovalBlocked::InstallationUnavailable,
        };
        return result;
    };
    let scope = write_scope(&input.grant);
    for retry in 0..2 {
        if let Err(error) = app.ensure(account.installation_id, scope).await {
            if let Some((_, retry_after)) = ghinvite_github::apps::rate_limit(&error) {
                result.limit = Some(crate::cancellation_protocol::content_limit(
                    now,
                    retry_after,
                ));
            }
            result.outcome = WriteOutcome::NoEffect {
                reason: RemovalBlocked::InstallationUnavailable,
            };
            return result;
        }
        let login = match observe(apps, &input.grant, account, now).await {
            Observation::Ready { login } => login,
            Observation::Absent => {
                result.outcome = WriteOutcome::Skipped {
                    reason: RemovalSkip::AlreadyAbsent,
                };
                return result;
            }
            Observation::Skip { role } => {
                result.outcome = WriteOutcome::Skipped {
                    reason: RemovalSkip::RoleChanged { now: role },
                };
                return result;
            }
            Observation::Blocked { reason } => {
                result.outcome = WriteOutcome::NoEffect { reason };
                return result;
            }
            Observation::Limited { limit } => {
                result.limit = Some(limit);
                result.outcome = WriteOutcome::NoEffect {
                    reason: RemovalBlocked::RateLimited,
                };
                return result;
            }
        };
        let generation = match storage.claim_removal_attempt(input).await {
            Ok(Some(value)) => value,
            _ => return result,
        };
        // Each HTTP attempt owns its own SQL generation. A replayed callback
        // would bypass the second fresh read and fence: one call, and a 401
        // has replaced the token before the loop's retry.
        let delete = app
            .write_once(account.installation_id, scope, |handle| {
                let login = &login;
                async move {
                    let rest = handle.rest();
                    match &input.grant.target {
                        GrantTarget::Repository { full_name, .. } => {
                            let identity =
                                ghinvite_core::RepositoryIdentity::parse(full_name.clone())
                                    .map_err(|_| {
                                        CallError::Api(ApiError::Decode {
                                            what: "removal_repository",
                                        })
                                    })?;
                            endpoints::remove_collaborator(
                                &rest,
                                Repository {
                                    owner: identity.owner(),
                                    name: identity.name(),
                                },
                                login,
                            )
                            .await?;
                        }
                        GrantTarget::Organization {
                            login: org_login, ..
                        } => org::remove_organization_member(&rest, org_login, login).await?,
                        GrantTarget::Team {
                            org_login, slug, ..
                        } => org::remove_team_membership(&rest, org_login, slug, login).await?,
                    }
                    Ok::<_, CallError<Infallible>>(())
                }
            })
            .await;
        match delete {
            Ok(()) => {
                result.outcome = WriteOutcome::Removed;
                return result;
            }
            Err(TokenCallError::Call(CallError::Api(ApiError::Unauthorized))) => {
                result.rejected_generation = Some(generation);
                result.outcome = WriteOutcome::NoEffect {
                    reason: RemovalBlocked::InstallationPermissions,
                };
                if retry == 0
                    && storage
                        .reject_removal_attempt(input, generation)
                        .await
                        .is_ok()
                {
                    // The first generation is positively rejected. Its result
                    // must not classify a later uncertain attempt as unused.
                    result.outcome = WriteOutcome::Unknown;
                    result.rejected_generation = None;
                    continue;
                }
                return result;
            }
            Err(TokenCallError::Call(CallError::Api(ApiError::RateLimited {
                retry_after,
                ..
            }))) => {
                result.limit = Some(crate::cancellation_protocol::content_limit(
                    now,
                    retry_after,
                ));
                result.rejected_generation = Some(generation);
                result.outcome = WriteOutcome::NoEffect {
                    reason: RemovalBlocked::RateLimited,
                };
                return result;
            }
            Err(TokenCallError::Call(CallError::Api(ApiError::Spammed))) => {
                let mut limit = crate::cancellation_protocol::content_limit(now, None);
                limit.observation.as_mut().expect("observed limit").reason =
                    ghinvite_core::budget::LimitReason::Spammed;
                result.limit = Some(limit);
                result.rejected_generation = Some(generation);
                result.outcome = WriteOutcome::NoEffect {
                    reason: RemovalBlocked::PolicyRefused,
                };
                return result;
            }
            Err(TokenCallError::Call(CallError::Api(
                ApiError::Forbidden { .. } | ApiError::Validation { .. } | ApiError::NotSent,
            ))) => {
                result.rejected_generation = Some(generation);
                result.outcome = WriteOutcome::NoEffect {
                    reason: RemovalBlocked::PolicyRefused,
                };
                return result;
            }
            _ => return result,
        }
    }
    result
}
