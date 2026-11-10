//! Read and write I/O stages. No Restate calls or credential-bearing values
//! cross this module's serializable stage results.
use crate::state::RepositoryGithub;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    Account, AccountType,
    budget::{BucketId, RecordLimit},
    org_delivery::*,
    storage::org_delivery::OrgDeliveryStorage,
};
use ghinvite_github::{
    AppSlot, GithubApp, TokenCallError,
    endpoints::{self, organization as api},
    error::{ApiError, CallError, ForbiddenReason},
    profiles::{ORG_INVITE, ORG_READ},
    rest::Rest,
    roles,
};
use octotoken::{TokenError, octoauth};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrganizationRead {
    Missing,
    Active {
        role: OrganizationRole,
    },
    Pending {
        upstream_id: u64,
        role: OrganizationInvitationRole,
        created_at: DateTime<Utc>,
        teams: Vec<u64>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeamRead {
    Missing,
    Present {
        role: TeamRole,
        state: MembershipState,
    },
    Blocked {
        reason: OrganizationBlockedReason,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ready {
    pub login: String,
    pub org_login: String,
    pub organization: OrganizationRead,
    pub teams: BTreeMap<u64, TeamRead>,
    pub seats_full: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreRead {
    Ready(Ready),
    Stopped {
        outcome: OrganizationCreateOutcome,
        limit: Option<RecordLimit>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepResult {
    Organization(OrganizationCreateOutcome),
    Team(TeamCreateOutcome),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    pub outcome: StepResult,
    pub unused: bool,
    pub rejected_generation: Option<u64>,
    pub limit: Option<RecordLimit>,
    pub recheck_permissions: bool,
}
pub(super) fn org_role(role: roles::OrgRole) -> Result<OrganizationRole, ApiError> {
    Ok(match role {
        roles::OrgRole::Member => OrganizationRole::Member,
        roles::OrgRole::Admin => OrganizationRole::Admin,
        _ => return Err(invalid("organization_role")),
    })
}
fn team_role(role: roles::TeamRole) -> Result<TeamRole, ApiError> {
    Ok(match role {
        roles::TeamRole::Member => TeamRole::Member,
        roles::TeamRole::Maintainer => TeamRole::Maintainer,
        _ => return Err(invalid("team_role")),
    })
}
fn membership(state: roles::MembershipState) -> Result<MembershipState, ApiError> {
    Ok(match state {
        roles::MembershipState::Active => MembershipState::Active,
        roles::MembershipState::Pending => MembershipState::Pending,
        _ => return Err(invalid("membership_state")),
    })
}
fn invitation_role(role: api::InvitationRole) -> Result<OrganizationInvitationRole, ApiError> {
    Ok(match role {
        api::InvitationRole::DirectMember => OrganizationInvitationRole::DirectMember,
        api::InvitationRole::Admin => OrganizationInvitationRole::Admin,
        api::InvitationRole::BillingManager => OrganizationInvitationRole::BillingManager,
        api::InvitationRole::HiringManager => OrganizationInvitationRole::HiringManager,
        _ => return Err(invalid("organization_invitation_role")),
    })
}
fn invalid(what: &'static str) -> ApiError {
    ApiError::Decode { what }
}
fn numeric(id: u64) -> bool {
    id > 0 && id <= ghinvite_core::storage::integer::MAX_EXACT
}
fn seats_full(plan: Option<&api::OrganizationPlan>) -> bool {
    plan.and_then(api::OrganizationPlan::paid_seats)
        .is_some_and(|(seats, used)| used >= seats)
}
pub(super) fn content_limit(now: DateTime<Utc>, wait: Option<std::time::Duration>) -> RecordLimit {
    RecordLimit::observed(
        BucketId::Content,
        Some(
            now + wait
                .and_then(|w| chrono::Duration::from_std(w).ok())
                .unwrap_or_else(|| chrono::Duration::days(1)),
        ),
        ghinvite_core::budget::LimitReason::RateLimited,
        now,
    )
}
fn api_limit(error: &ApiError, now: DateTime<Utc>) -> Option<RecordLimit> {
    match error {
        ApiError::RateLimited { retry_after, .. } => Some(content_limit(now, *retry_after)),
        _ => None,
    }
}
pub(super) fn stopped(error: crate::HandlerError, now: DateTime<Utc>) -> PreRead {
    let limit = error
        .rate_limit()
        .map(|l| content_limit(now, l.retry_after));
    let reason = match &error {
        crate::HandlerError::GithubApi(ApiError::Decode {
            what: "organization_requester_identity",
        }) => OrganizationBlockedReason::RequesterIdentity,
        crate::HandlerError::GithubApi(ApiError::Forbidden {
            reason: ForbiddenReason::IpAllowList,
        }) => OrganizationBlockedReason::IpAllowList,
        crate::HandlerError::Token(TokenError::GitHub(octoauth::Error::Forbidden { .. })) => {
            OrganizationBlockedReason::MintForbidden
        }
        _ => OrganizationBlockedReason::TargetUnavailable,
    };
    let outcome = match &limit {
        Some(limit) => OrganizationCreateOutcome::Throttled {
            until: limit.until.expect("bounded limit"),
            reason: None,
        },
        None => OrganizationCreateOutcome::Blocked { reason },
    };
    PreRead::Stopped { outcome, limit }
}
pub(super) async fn verify_installation(
    apps: &RepositoryGithub,
    command: &OrganizationCreateCommand,
    account: &Account,
) -> crate::Result<GithubApp> {
    if account.account_id != command.account_id
        || account.account_type != AccountType::Organization
        || account.uninstalled_at.is_some()
    {
        return Err(invalid("organization_installation_identity").into());
    }
    let app = apps
        .apps
        .app(AppSlot::Org)
        .map_err(|_| invalid("organization_app_missing"))?;
    let credential = app.app_rest().map_err(TokenError::from)?;
    let actual = endpoints::get_installation(&credential.rest(), account.installation_id).await?;
    if actual.id != account.installation_id
        || actual.account.id != command.account_id
        || actual.suspended_at.is_some()
        || app.id().get() != actual.app_id
    {
        return Err(invalid("organization_installation_identity").into());
    }
    Ok(app)
}
pub(super) async fn requester(rest: &Rest<'_>, id: u64) -> Result<String, ApiError> {
    let user = endpoints::get_user_by_id(rest, id).await?;
    if user.id != id || user.login.is_empty() {
        return Err(invalid("organization_requester_identity"));
    }
    let bound = endpoints::get_user_by_login(rest, &user.login).await?;
    if bound.id != id || !bound.login.eq_ignore_ascii_case(&user.login) {
        return Err(invalid("organization_requester_identity"));
    }
    Ok(bound.login)
}
pub(super) async fn org_membership(
    rest: &Rest<'_>,
    command: &OrganizationCreateCommand,
    org: &str,
    login: &str,
) -> Result<Option<api::OrganizationMembership>, ApiError> {
    match api::organization_membership(rest, org, login).await {
        Ok(value)
            if value.user.id == command.requester_id
                && value.organization.id == command.account_id
                && value.user.login.eq_ignore_ascii_case(login) =>
        {
            org_role(value.role)?;
            membership(value.state)?;
            Ok(Some(value))
        }
        Ok(_) => Err(invalid("organization_requester_identity")),
        Err(ApiError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}
/// A list entry's ID is an invitation ID, never a user ID. A complete unique
/// login match is bound to the numeric requester again after the list walk.
async fn pending_invitation(
    rest: &Rest<'_>,
    command: &OrganizationCreateCommand,
    org: &str,
    login: &str,
) -> Result<api::OrganizationInvitation, ApiError> {
    let all = api::list_invitations(rest, org).await?;
    let mut ids = BTreeSet::new();
    if all.iter().any(|i| !numeric(i.id) || !ids.insert(i.id)) {
        return Err(invalid("organization_invitation_list"));
    }
    let mut matches = all.into_iter().filter(|i| {
        i.login
            .as_deref()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(login))
    });
    let found = matches
        .next()
        .ok_or_else(|| invalid("organization_invitation_missing"))?;
    if matches.next().is_some() {
        return Err(invalid("organization_invitation_ambiguous"));
    }
    let user = endpoints::get_user_by_login(rest, login).await?;
    if user.id != command.requester_id {
        return Err(invalid("organization_requester_identity"));
    }
    Ok(found)
}
pub(super) async fn team_membership(
    rest: &Rest<'_>,
    org: &str,
    target: &ghinvite_core::targets::TeamTarget,
    login: &str,
) -> Result<TeamRead, ApiError> {
    let team = match api::get_team(rest, org, &target.team_slug).await {
        Ok(team) => team,
        Err(ApiError::NotFound) => {
            return Ok(TeamRead::Blocked {
                reason: OrganizationBlockedReason::TargetUnavailable,
            });
        }
        Err(error) => return Err(error),
    };
    if team.id != target.team_id || !team.slug.eq_ignore_ascii_case(&target.team_slug) {
        return Ok(TeamRead::Blocked {
            reason: OrganizationBlockedReason::TargetUnavailable,
        });
    }
    match api::team_membership(rest, org, &team.slug, login).await {
        Ok(value) => Ok(TeamRead::Present {
            role: team_role(value.role)?,
            state: membership(value.state)?,
        }),
        Err(ApiError::NotFound) if team.externally_managed => Ok(TeamRead::Blocked {
            reason: OrganizationBlockedReason::IdpSyncedTeam,
        }),
        Err(ApiError::NotFound) => Ok(TeamRead::Missing),
        Err(error) => Err(error),
    }
}
pub async fn pre_read(
    apps: &RepositoryGithub,
    command: &OrganizationCreateCommand,
    account: &Account,
    now: DateTime<Utc>,
) -> PreRead {
    match pre_read_inner(apps, command, account, true).await {
        Ok(ready) => PreRead::Ready(ready),
        Err(error) => stopped(error, now),
    }
}
/// Cancellation already owns the immutable invitation receipt. Verify live
/// organization, requester, installation and invitation identity without making
/// deletion depend on the continued existence or visibility of attached teams.
pub(super) async fn cancellation_read(
    apps: &RepositoryGithub,
    command: &OrganizationCreateCommand,
    account: &Account,
    now: DateTime<Utc>,
) -> PreRead {
    match pre_read_inner(apps, command, account, false).await {
        Ok(ready) => PreRead::Ready(ready),
        Err(error) => stopped(error, now),
    }
}
async fn pre_read_inner(
    apps: &RepositoryGithub,
    command: &OrganizationCreateCommand,
    account: &Account,
    include_teams: bool,
) -> crate::Result<Ready> {
    let app = verify_installation(apps, command, account).await?;
    app.read(account.installation_id, &ORG_READ, |handle| async move {
        let rest = handle.rest();
        let org = api::get_organization(&rest, &account.account_login).await?;
        if org.id != command.account_id {
            return Err(CallError::Api(invalid("organization_identity")));
        }
        let login = requester(&rest, command.requester_id).await?;
        let observed = org_membership(&rest, command, &org.login, &login).await?;
        let organization = match observed {
            None => OrganizationRead::Missing,
            Some(value) if value.state == roles::MembershipState::Active => {
                OrganizationRead::Active {
                    role: org_role(value.role)?,
                }
            }
            Some(_) => {
                let invitation = pending_invitation(&rest, command, &org.login, &login).await?;
                let teams = if include_teams {
                    api::list_invitation_teams(&rest, &org.login, invitation.id).await?
                } else {
                    Vec::new()
                };
                let mut ids = BTreeSet::new();
                if teams.iter().any(|t| !numeric(t.id) || !ids.insert(t.id))
                    || (include_teams && teams.len() as u64 != invitation.team_count)
                {
                    return Err(CallError::Api(invalid("organization_invitation_teams")));
                }
                OrganizationRead::Pending {
                    upstream_id: invitation.id,
                    role: invitation_role(invitation.role)?,
                    created_at: invitation.created_at,
                    teams: ids.into_iter().collect(),
                }
            }
        };
        let mut teams = BTreeMap::new();
        for target in command.teams.iter().filter(|_| include_teams) {
            let observed = match team_membership(&rest, &org.login, target, &login).await {
                Ok(observed) => observed,
                Err(error @ ApiError::RateLimited { .. }) => return Err(CallError::Api(error)),
                Err(_) => TeamRead::Blocked {
                    reason: OrganizationBlockedReason::TargetUnavailable,
                },
            };
            teams.insert(target.team_id, observed);
        }
        let seats_full = seats_full(org.plan.as_ref());
        Ok::<_, CallError<Infallible>>(Ready {
            login,
            org_login: org.login,
            organization,
            teams,
            seats_full,
        })
    })
    .await
    .map_err(Into::into)
}
pub fn organization_plan(read: &Ready) -> Option<OrganizationCreateOutcome> {
    match read.organization {
        OrganizationRead::Active { role } => {
            Some(OrganizationCreateOutcome::AlreadySatisfied { role })
        }
        OrganizationRead::Pending {
            upstream_id, role, ..
        } => Some(OrganizationCreateOutcome::AlreadyPending { upstream_id, role }),
        OrganizationRead::Missing if read.seats_full => Some(OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat,
        }),
        OrganizationRead::Missing
            if read
                .teams
                .values()
                .any(|t| matches!(t, TeamRead::Blocked { .. })) =>
        {
            Some(OrganizationCreateOutcome::Blocked {
                reason: OrganizationBlockedReason::TargetUnavailable,
            })
        }
        OrganizationRead::Missing => None,
    }
}
pub fn team_plan(read: &TeamRead) -> Option<TeamCreateOutcome> {
    match read {
        TeamRead::Present { role, state } => Some(TeamCreateOutcome::AlreadySatisfied {
            role: *role,
            state: *state,
        }),
        TeamRead::Blocked { reason } => Some(TeamCreateOutcome::Blocked { reason: *reason }),
        TeamRead::Missing => None,
    }
}

fn wrapped(
    action: &OrganizationWrite,
    org: OrganizationCreateOutcome,
    team: TeamCreateOutcome,
) -> StepResult {
    if matches!(action, OrganizationWrite::Team { .. }) {
        StepResult::Team(team)
    } else {
        StepResult::Organization(org)
    }
}
fn blocked(action: &OrganizationWrite, reason: OrganizationBlockedReason) -> StepResult {
    wrapped(
        action,
        OrganizationCreateOutcome::Blocked { reason },
        TeamCreateOutcome::Blocked { reason },
    )
}
fn positive_policy(outcome: &StepResult) -> bool {
    matches!(
        outcome,
        StepResult::Organization(OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat | OrganizationBlockedReason::IdpSyncedTeam,
        }) | StepResult::Team(TeamCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::IdpSyncedTeam,
        })
    )
}

fn unknown(action: &OrganizationWrite) -> StepResult {
    wrapped(
        action,
        OrganizationCreateOutcome::OutcomeUnknown,
        TeamCreateOutcome::OutcomeUnknown,
    )
}
fn throttled(action: &OrganizationWrite, until: DateTime<Utc>) -> StepResult {
    wrapped(
        action,
        OrganizationCreateOutcome::Throttled {
            until,
            reason: None,
        },
        TeamCreateOutcome::Throttled {
            until,
            reason: None,
        },
    )
}
fn failed(action: &OrganizationWrite, status: u16) -> StepResult {
    let reason = ghinvite_core::delivery::FailureReason::Rejected { status };
    wrapped(
        action,
        OrganizationCreateOutcome::Failed { reason },
        TeamCreateOutcome::Failed { reason },
    )
}
fn mint_result(action: &OrganizationWrite, error: TokenError, now: DateTime<Utc>) -> WriteResult {
    let mut limit = None;
    let mut recheck_permissions = false;
    let outcome = match ghinvite_github::apps::github_error(&error) {
        Some(octoauth::Error::RateLimited { retry_after, .. }) => {
            let value = content_limit(now, *retry_after);
            let outcome = throttled(action, value.until.expect("bounded limit"));
            limit = Some(value);
            outcome
        }
        Some(octoauth::Error::PermissionsNotGranted) => {
            recheck_permissions = true;
            blocked(action, OrganizationBlockedReason::InstallationPermissions)
        }
        Some(octoauth::Error::InstallationGone | octoauth::Error::InstallationSuspended) => {
            blocked(action, OrganizationBlockedReason::InstallationUnavailable)
        }
        Some(octoauth::Error::Forbidden {
            reason: octoauth::ForbiddenReason::IpAllowList,
        }) => blocked(action, OrganizationBlockedReason::IpAllowList),
        Some(octoauth::Error::Forbidden { .. }) => {
            blocked(action, OrganizationBlockedReason::MintForbidden)
        }
        _ => blocked(action, OrganizationBlockedReason::TargetUnavailable),
    };
    WriteResult {
        outcome,
        unused: true,
        rejected_generation: None,
        limit,
        recheck_permissions,
    }
}
async fn before_write(
    apps: &RepositoryGithub,
    input: &OrganizationAttempt,
    account: &Account,
) -> crate::Result<(Option<StepResult>, String, String)> {
    let app = verify_installation(apps, &input.create, account).await?;
    app.read(account.installation_id, &ORG_READ, |handle| async move {
        let rest = handle.rest();
        let org = api::get_organization(&rest, &account.account_login).await?;
        if org.id != input.create.account_id {
            return Err(CallError::Api(invalid("organization_identity")));
        }
        let login = requester(&rest, input.create.requester_id).await?;
        let result = match input.action {
            OrganizationWrite::Invite => {
                match org_membership(&rest, &input.create, &org.login, &login).await? {
                    None => {
                        if seats_full(org.plan.as_ref()) {
                            Some(blocked(&input.action, OrganizationBlockedReason::NoSeat))
                        } else {
                            // Every team ID in the one POST is bound to its current
                            // org/slug immediately before the fence too.
                            for target in &input.create.teams {
                                let team =
                                    api::get_team(&rest, &org.login, &target.team_slug).await?;
                                if team.id != target.team_id {
                                    return Err(CallError::Api(invalid(
                                        "organization_team_identity",
                                    )));
                                }
                                if team.externally_managed {
                                    return Ok((
                                        Some(blocked(
                                            &input.action,
                                            OrganizationBlockedReason::IdpSyncedTeam,
                                        )),
                                        login,
                                        org.login,
                                    ));
                                }
                            }
                            None
                        }
                    }
                    Some(value) if value.state == roles::MembershipState::Active => Some(
                        StepResult::Organization(OrganizationCreateOutcome::AlreadySatisfied {
                            role: org_role(value.role)?,
                        }),
                    ),
                    Some(_) => {
                        let pending =
                            pending_invitation(&rest, &input.create, &org.login, &login).await?;
                        Some(StepResult::Organization(
                            OrganizationCreateOutcome::AlreadyPending {
                                upstream_id: pending.id,
                                role: invitation_role(pending.role)?,
                            },
                        ))
                    }
                }
            }
            OrganizationWrite::Team { team_id } => {
                let target = input
                    .create
                    .teams
                    .iter()
                    .find(|t| t.team_id == team_id)
                    .ok_or_else(|| invalid("organization_team_identity"))?;
                team_plan(&team_membership(&rest, &org.login, target, &login).await?)
                    .map(StepResult::Team)
            }
            OrganizationWrite::Cancel { .. } => {
                return Err(CallError::Api(invalid("organization_create_action")));
            }
        };
        Ok::<_, CallError<Infallible>>((result, login, org.login))
    })
    .await
    .map_err(Into::into)
}
/// S4 never calls another Restate object. Slots are reserved by the owner before
/// this stage and its result is retained before any accounting RPC.
pub async fn write(
    apps: &RepositoryGithub,
    storage: &dyn OrgDeliveryStorage,
    input: &OrganizationAttempt,
    account: &Account,
    now: DateTime<Utc>,
) -> WriteResult {
    let mut result = WriteResult {
        outcome: unknown(&input.action),
        unused: false,
        rejected_generation: None,
        limit: None,
        recheck_permissions: false,
    };
    if input.validate().is_err() || matches!(input.action, OrganizationWrite::Cancel { .. }) {
        return result;
    }
    if storage
        .organization_attempt_exists(input)
        .await
        .unwrap_or(true)
    {
        return result;
    }
    let app = match apps.apps.app(AppSlot::Org) {
        Ok(app) => app,
        Err(_) => {
            let gone = TokenError::GitHub(octoauth::Error::InstallationGone);
            return mint_result(&input.action, gone, now);
        }
    };
    for retry in 0..2 {
        if let Err(error) = app.ensure(account.installation_id, &ORG_INVITE).await {
            let mut failed = mint_result(&input.action, error, now);
            failed.rejected_generation = result.rejected_generation;
            return failed;
        }
        let (login, org_login) = match before_write(apps, input, account).await {
            Ok((Some(outcome), _, _)) => {
                result.outcome = outcome;
                result.unused = true;
                return result;
            }
            Ok((None, login, org_login)) => (login, org_login),
            Err(error) => {
                let PreRead::Stopped { outcome, limit } = stopped(error, now) else {
                    unreachable!()
                };
                result.outcome = match outcome {
                    OrganizationCreateOutcome::Throttled { until, .. } => {
                        throttled(&input.action, until)
                    }
                    OrganizationCreateOutcome::Blocked { reason } => blocked(&input.action, reason),
                    _ => unreachable!(),
                };
                result.limit = limit;
                result.unused = true;
                return result;
            }
        };
        let generation = match storage.claim_organization_attempt(input).await {
            Ok(Some(generation)) => generation,
            _ => return result,
        };
        let attempted = app
            .write_once(account.installation_id, &ORG_INVITE, |handle| {
                let login = login.clone();
                let org_login = org_login.clone();
                async move {
                    let rest = handle.rest();
                    let outcome = match input.action {
                        OrganizationWrite::Invite => {
                            let invitation = api::create_invitation(
                                &rest,
                                &org_login,
                                &api::OrganizationInvitationInput {
                                    invitee: api::Invitee::User {
                                        invitee_id: input.create.requester_id,
                                    },
                                    role: api::CreateInvitationRole::DirectMember,
                                    team_ids: input
                                        .create
                                        .teams
                                        .iter()
                                        .map(|t| t.team_id)
                                        .collect(),
                                },
                            )
                            .await?;
                            if !numeric(invitation.id)
                                || !invitation.login.as_deref().is_some_and(|returned_login| {
                                    returned_login.eq_ignore_ascii_case(&login)
                                })
                                || invitation.role != api::InvitationRole::DirectMember
                                || invitation.team_count != input.create.teams.len() as u64
                                || invitation.created_at.timestamp()
                                    < input.create.approved_at.timestamp()
                            {
                                return Err(CallError::Api(invalid(
                                    "organization_create_response",
                                )));
                            }
                            StepResult::Organization(OrganizationCreateOutcome::Created {
                                upstream_id: invitation.id,
                                created_at: invitation.created_at,
                            })
                        }
                        OrganizationWrite::Team { team_id } => {
                            let target = input
                                .create
                                .teams
                                .iter()
                                .find(|t| t.team_id == team_id)
                                .expect("validated team");
                            let membership = api::put_team_membership(
                                &rest,
                                &org_login,
                                &target.team_slug,
                                &login,
                                roles::TeamRole::Member,
                            )
                            .await?;
                            // Future role variants cannot become a confirmed built-in grant.
                            team_role(membership.role)?;
                            StepResult::Team(match membership.state {
                                roles::MembershipState::Active => TeamCreateOutcome::Upgraded,
                                roles::MembershipState::Pending => TeamCreateOutcome::Created {
                                    via: TeamCreation::PendingMembership,
                                },
                                _ => return Err(CallError::Api(invalid("team_membership_state"))),
                            })
                        }
                        OrganizationWrite::Cancel { .. } => unreachable!("create action validated"),
                    };
                    Ok::<_, CallError<Infallible>>(outcome)
                }
            })
            .await;
        match attempted {
            Ok(outcome) => {
                result.outcome = outcome;
                return result;
            }
            Err(TokenCallError::Call(CallError::Api(ApiError::Unauthorized))) => {
                result.rejected_generation = Some(generation);
                if storage
                    .reject_organization_attempt(input, generation)
                    .await
                    .is_err()
                {
                    result.unused = true;
                    result.outcome = blocked(
                        &input.action,
                        OrganizationBlockedReason::InstallationUnavailable,
                    );
                    return result;
                }
                if retry == 0 {
                    continue;
                }
                result.unused = true;
                result.recheck_permissions = true;
                result.outcome = blocked(
                    &input.action,
                    OrganizationBlockedReason::InstallationUnavailable,
                );
                return result;
            }
            Err(TokenCallError::Call(CallError::Api(error))) => {
                let positive = matches!(
                    error,
                    ApiError::Forbidden { .. }
                        | ApiError::NotFound
                        | ApiError::Conflict
                        | ApiError::NotAllowed
                        | ApiError::Gone
                        | ApiError::Validation { .. }
                        | ApiError::Spammed
                        | ApiError::RateLimited { .. }
                        | ApiError::NotSent
                );
                if !positive {
                    return result;
                }
                result.unused = true;
                result.rejected_generation = Some(generation);
                result.limit = api_limit(&error, now);
                // A rejection status alone is not a policy classifier. A fresh
                // numeric-bound read can establish a restriction that appeared
                // after the final pre-write check; unknown prose stays generic.
                let inspect_policy = matches!(
                    &error,
                    ApiError::Validation { .. }
                        | ApiError::Forbidden {
                            reason: ForbiddenReason::Other | ForbiddenReason::Permission
                        }
                );
                result.outcome = match error {
                    ApiError::RateLimited { .. } => throttled(
                        &input.action,
                        result
                            .limit
                            .as_ref()
                            .and_then(|l| l.until)
                            .expect("bounded limit"),
                    ),
                    ApiError::Spammed => {
                        let until = now + chrono::Duration::days(1);
                        result.limit = Some(RecordLimit::observed(
                            if matches!(input.action, OrganizationWrite::Invite) {
                                BucketId::Org
                            } else {
                                BucketId::Content
                            },
                            Some(until),
                            ghinvite_core::budget::LimitReason::Spammed,
                            now,
                        ));
                        throttled(&input.action, until)
                    }
                    ApiError::Forbidden {
                        reason: ForbiddenReason::IpAllowList,
                    } => blocked(&input.action, OrganizationBlockedReason::IpAllowList),
                    ApiError::Forbidden { .. } => blocked(
                        &input.action,
                        OrganizationBlockedReason::InstallationUnavailable,
                    ),
                    ApiError::NotFound | ApiError::Gone => {
                        blocked(&input.action, OrganizationBlockedReason::TargetUnavailable)
                    }
                    ApiError::NotSent => {
                        blocked(&input.action, OrganizationBlockedReason::TargetUnavailable)
                    }
                    ApiError::Validation { .. } => failed(&input.action, 422),
                    ApiError::Conflict => failed(&input.action, 409),
                    ApiError::NotAllowed => failed(&input.action, 405),
                    _ => unreachable!("positive status classified"),
                };
                if inspect_policy {
                    match before_write(apps, input, account).await {
                        Ok((Some(outcome), _, _)) if positive_policy(&outcome) => {
                            result.outcome = outcome;
                        }
                        Err(error) => {
                            // The rejected write is still positively unused.
                            // Retain any new read limit before owner accounting,
                            // without turning unavailable facts into a policy.
                            if let Some(limit) = error.rate_limit() {
                                result.limit = Some(content_limit(now, limit.retry_after));
                            }
                        }
                        _ => {}
                    }
                }
                return result;
            }
            _ => return result,
        }
    }
    result
}

#[cfg(test)]
mod eligibility_tests {
    use super::*;
    #[test]
    fn seat_refusal_requires_positive_paid_capacity_facts() {
        let plan = |name: &str, seats, filled_seats| api::OrganizationPlan {
            name: name.into(),
            seats,
            filled_seats,
        };
        assert!(!seats_full(None));
        assert!(!seats_full(Some(&plan("free", Some(10), Some(10)))));
        assert!(!seats_full(Some(&plan("team", None, Some(10)))));
        assert!(!seats_full(Some(&plan("enterprise", Some(0), Some(0)))));
        assert!(!seats_full(Some(&plan("enterprise", Some(10), Some(9)))));
        assert!(seats_full(Some(&plan("enterprise", Some(10), Some(10)))));
        assert!(seats_full(Some(&plan("team", Some(10), Some(11)))));
    }
}
