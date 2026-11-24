//! Complete-list observations are combined with fresh numeric identity and
//! membership evidence. Webhooks only wake this path; they never settle it.
use super::*;
use ghinvite_core::{InvitationState, org_observation::OrganizationObservation};
use ghinvite_github::{
    endpoints::organization as api,
    error::{ApiError, CallError},
    profiles::ORG_READ,
    roles::MembershipState as GhMembershipState,
};
use std::{collections::BTreeSet, convert::Infallible};

async fn read(
    apps: &crate::RepositoryGithub,
    command: &OrganizationCreateCommand,
    account: &ghinvite_core::Account,
    input: &OrganizationObservation,
    recover_owned: bool,
) -> crate::Result<protocol::Ready> {
    let app = protocol::verify_installation(apps, command, account).await?;
    app.read(account.installation_id, &ORG_READ, |handle| async move {
        let rest = handle.rest();
        let org = api::get_organization(&rest, &account.account_login).await?;
        if org.id != command.account_id {
            return Err(CallError::Api(ApiError::Decode {
                what: "organization_identity",
            }));
        }
        let login = protocol::requester(&rest, command.requester_id).await?;
        let member = protocol::org_membership(&rest, command, &org.login, &login).await?;
        let organization = if member
            .as_ref()
            .is_some_and(|m| m.state == GhMembershipState::Active)
        {
            protocol::OrganizationRead::Active {
                role: protocol::org_role(member.expect("active membership").role)?,
            }
        } else {
            let mut matches = input.pending.iter().filter(|i| {
                i.login
                    .as_ref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&login))
            });
            match matches.next() {
                Some(invite) => {
                    if matches.next().is_some() {
                        return Err(CallError::Api(ApiError::Decode {
                            what: "organization_invitation_ambiguous",
                        }));
                    }
                    let teams = if recover_owned {
                        let teams =
                            api::list_invitation_teams(&rest, &org.login, invite.upstream_id)
                                .await?;
                        let ids: BTreeSet<_> = teams.iter().map(|t| t.id).collect();
                        if ids.len() != teams.len()
                            || teams.len() as u64 != invite.team_count
                            || ids.iter().any(|id| {
                                *id == 0 || *id > ghinvite_core::storage::integer::MAX_EXACT
                            })
                        {
                            return Err(CallError::Api(ApiError::Decode {
                                what: "organization_invitation_teams",
                            }));
                        }
                        ids.into_iter().collect()
                    } else {
                        Vec::new()
                    };
                    protocol::OrganizationRead::Pending {
                        upstream_id: invite.upstream_id,
                        role: invite.role,
                        created_at: invite.created_at,
                        teams,
                    }
                }
                None => protocol::OrganizationRead::Missing,
            }
        };
        let mut teams = BTreeMap::new();
        for target in &command.teams {
            let observed = match protocol::team_membership(&rest, &org.login, target, &login).await
            {
                Ok(value) => value,
                Err(error @ ApiError::RateLimited { .. }) => return Err(CallError::Api(error)),
                Err(_) => protocol::TeamRead::Blocked {
                    reason: OrganizationBlockedReason::TargetUnavailable,
                },
            };
            teams.insert(target.team_id, observed);
        }
        Ok::<_, CallError<Infallible>>(protocol::Ready {
            login,
            org_login: org.login,
            organization,
            teams,
            seats_full: false,
        })
    })
    .await
    .map_err(Into::into)
}

pub(super) async fn listed(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    input: OrganizationObservation,
) -> Result<(), TerminalError> {
    input.validate().map_err(|_| conflict())?;
    let Some(Json(mut value)) = ctx.get::<Json<Retained>>("organization").await? else {
        return Ok(());
    };
    let command = value.snapshot.create.command.clone();
    if command.delivery_key() != ctx.key()
        || input.account_id != command.account_id
        || input.org_id != command.organization.org_id
    {
        return Err(conflict());
    }
    let now = crate::clock::now(ctx, "organization_observation_time").await?;
    if !value.snapshot.needs_tracking()
        || input.observed_at > now
        || input.observation_started_at < command.approved_at
        || value
            .last_observed_at
            .is_some_and(|at| input.observation_started_at < at)
    {
        return Ok(());
    }
    let account_key = format!("org:{}", command.account_id);
    let Some(account) = ctx
        .object_client::<crate::availability::AccountInstallationClient>(&account_key)
        .current_installation()
        .call()
        .await?
        .0
    else {
        return Ok(());
    };
    if account.installation_id != input.installation_id {
        return Ok(());
    }
    let apps = owner
        .state
        .github_apps
        .as_ref()
        .ok_or_else(|| TerminalError::new_with_code(503, "organization app unavailable"))?;
    let recover_owned = !confirmed(&value, "org") && value.steps["org"].may_have_written;
    let Json(result) = ctx
        .run(|| async {
            Ok::<_, HandlerError>(Json(
                match read(apps, &command, &account, &input, recover_owned).await {
                    Ok(read) => protocol::PreRead::Ready(read),
                    Err(error) => protocol::stopped(error, now),
                },
            ))
        })
        .name("organization_observe_memberships")
        .await?;
    let read = match result {
        protocol::PreRead::Ready(read) => read,
        protocol::PreRead::Stopped { limit, .. } => {
            if let Some(limit) = limit {
                owner.record_limit(ctx, &value, limit).await?;
            }
            return Ok(());
        }
    };
    let completed = crate::clock::now(ctx, "organization_observation_completed").await?;
    value.last_observed_at = Some(completed);
    telemetry::memberships(&mut value, &read, input.observation_started_at);
    if recover_owned {
        let outcome = match &read.organization {
            protocol::OrganizationRead::Pending {
                upstream_id,
                role: OrganizationInvitationRole::DirectMember,
                created_at,
                teams,
            } if value.steps["org"]
                .started_at
                .is_some_and(|at| created_at.timestamp() >= at.timestamp())
                && *created_at <= input.observation_started_at
                && teams == &command.teams.iter().map(|t| t.team_id).collect::<Vec<_>>() =>
            {
                Some(OrganizationCreateOutcome::Created {
                    upstream_id: *upstream_id,
                    created_at: *created_at,
                })
            }
            _ => None,
        };
        if let Some(outcome) = outcome {
            set_step(
                &mut value,
                "org",
                protocol::StepResult::Organization(outcome),
                completed,
            )?;
            if let OrganizationCreateOutcome::Created { upstream_id, .. } =
                value.snapshot.create.outcome
            {
                for target in &command.teams {
                    set_step(
                        &mut value,
                        &format!("team:{}", target.team_id),
                        protocol::StepResult::Team(TeamCreateOutcome::Created {
                            via: TeamCreation::OrganizationInvitation { upstream_id },
                        }),
                        completed,
                    )?;
                }
            }
        }
    }
    if let Some(upstream_id) = value.snapshot.create.outcome.upstream_id() {
        telemetry::invitation(&mut value, upstream_id, input.observation_started_at);
    }
    if let Some(id) = value.snapshot.create.outcome.upstream_id()
        && let Some(listed) = input.pending.iter().find(|i| i.upstream_id == id)
        && let Some(timing) = ghinvite_core::delivery::InvitationTiming::observed(
            id,
            listed.created_at,
            input.observed_at,
            command.approved_at,
            matches!(
                value.snapshot.create.outcome,
                OrganizationCreateOutcome::Created { .. }
            ),
        )
    {
        value.snapshot.retain_invitation_timing(timing);
    }
    let uncertain_cancel = cancellation::uncertain(ctx).await?;
    if let Some(upstream_id) = value.snapshot.create.outcome.upstream_id()
        && value.snapshot.settlement.is_none()
        && value.snapshot.tracking_stopped.is_none()
        && value
            .snapshot
            .create
            .confirmed_at
            .is_some_and(|at| input.observation_started_at >= at)
    {
        let state = if input.expired(upstream_id) {
            // Still listed past GitHub's 7-day lifetime in this complete,
            // fresh observation (#86). Absence below is never expiry.
            Some(InvitationState::Expired)
        } else if input.pending.iter().any(|i| i.upstream_id == upstream_id)
            || input
                .failed
                .iter()
                .any(|i| i.invitation.upstream_id == upstream_id)
        {
            // Complete-list presence wins over a later contradictory membership
            // read. Absence is the prerequisite for acceptance/decline here.
            None
        } else if matches!(read.organization, protocol::OrganizationRead::Active { .. }) {
            Some(InvitationState::Accepted)
        } else if uncertain_cancel {
            None
        } else {
            Some(InvitationState::Declined)
        };
        if let Some(state) = state {
            cancellation::settle(ctx, &mut value, state, completed, None).await?;
        }
    }
    for target in &command.teams {
        let step = format!("team:{}", target.team_id);
        let observed = &read.teams[&target.team_id];
        if !confirmed(&value, &step)
            && value.steps[&step].may_have_written
            && let Some(outcome) = protocol::team_plan(observed)
            && matches!(outcome, TeamCreateOutcome::AlreadySatisfied { .. })
        {
            set_step(
                &mut value,
                &step,
                protocol::StepResult::Team(outcome),
                completed,
            )?;
        }
        let team = value
            .snapshot
            .teams
            .get_mut(&target.team_id)
            .expect("command team");
        if team.settlement.is_some()
            || team.tracking_stopped.is_some()
            || team
                .create
                .confirmed_at
                .is_none_or(|at| input.observation_started_at < at)
        {
            continue;
        }
        let settled = match (&team.create.outcome, observed) {
            (
                TeamCreateOutcome::Created {
                    via: TeamCreation::PendingMembership,
                },
                protocol::TeamRead::Present {
                    state: MembershipState::Active,
                    ..
                },
            ) => Some(InvitationState::Accepted),
            (
                TeamCreateOutcome::Created {
                    via: TeamCreation::PendingMembership,
                },
                protocol::TeamRead::Missing,
            ) => Some(InvitationState::Declined),
            _ => None,
        };
        if let Some(state) = settled {
            team.settlement = Some(TeamSettlement {
                state,
                observed_at: completed,
                reason: None,
            });
            team.revision = increment(team.revision)?;
            value.snapshot.revision = increment(value.snapshot.revision)?;
        }
    }
    value.snapshot.validate().map_err(|_| conflict())?;
    save(ctx, &mut value).await?;
    if let Some(Json(inbox)) = ctx.get::<Json<Inbox>>("control").await? {
        control::advance(owner, ctx, &mut value, &inbox, completed).await?;
    }
    project(ctx, &mut value).await?;
    Ok(())
}
