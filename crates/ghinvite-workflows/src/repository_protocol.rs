//! Repository Delivery's two external-I/O stages (P1 §7.3).
//!
//! The owner journals `pre_read`, decides with `plan`, then journals `write`.
//! No Restate call belongs in either I/O stage. The owner reserves its M1 budget
//! between them. Credentials never appear in these serializable stage results.
use ghinvite_core::{
    RepositoryIdentity,
    delivery::{BlockedReason, CreateCommand, CreateOutcome, FailureReason},
    storage::DeliveryStorage,
    targets::{CustomRoleName, EffectiveRole, RepoRole},
};
use ghinvite_github::{
    GithubApp, TokenCallError,
    apps::{github_error, rate_limit},
    endpoints::{self, CollaboratorWrite, Repository},
    error::{ApiError, CallError, ForbiddenReason, ValidationCode},
    profiles,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvitation {
    #[serde(default)]
    pub timing: Option<ghinvite_core::delivery::InvitationTiming>,
    pub upstream_id: u64,
    pub permission: EffectiveRole,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreRead {
    Ready {
        login: String,
        role: EffectiveRole,
        pending: Option<PendingInvitation>,
    },
    Stopped(CreateOutcome),
    Limited {
        outcome: CreateOutcome,
        limit: ghinvite_core::budget::RecordLimit,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan {
    Complete(CreateOutcome),
    Put,
    Patch { upstream_id: u64 },
}

/// S2: compare only built-in ranks; never lower a grant or a pending invitation.
pub fn plan(read: &PreRead, grant: RepoRole) -> Plan {
    let PreRead::Ready { role, pending, .. } = read else {
        let (PreRead::Stopped(outcome) | PreRead::Limited { outcome, .. }) = read else {
            unreachable!()
        };
        return Plan::Complete(outcome.clone());
    };
    if let Some(outcome) = satisfied(role, grant) {
        return Plan::Complete(outcome);
    }
    if let Some(pending) = pending {
        if let EffectiveRole::Custom(name) = &pending.permission {
            return Plan::Complete(CreateOutcome::SkippedCustomRole {
                role_name: name.clone(),
            });
        }
        if pending.permission.satisfies(grant) {
            let permission = builtin(&pending.permission).expect("satisfying role is a grant");
            return Plan::Complete(CreateOutcome::AlreadyPending {
                upstream_id: pending.upstream_id,
                permission,
            });
        }
        // Missing/unrankable pending permission cannot authorize a mutation.
        if builtin(&pending.permission).is_none() {
            return Plan::Complete(blocked(BlockedReason::TargetUnavailable));
        }
        return Plan::Patch {
            upstream_id: pending.upstream_id,
        };
    }
    Plan::Put
}

fn satisfied(role: &EffectiveRole, grant: RepoRole) -> Option<CreateOutcome> {
    match role {
        EffectiveRole::Custom(name) => Some(CreateOutcome::SkippedCustomRole {
            role_name: name.clone(),
        }),
        role if role.satisfies(grant) => Some(CreateOutcome::AlreadySatisfied {
            effective_role: role.clone(),
        }),
        _ => None,
    }
}
fn builtin(role: &EffectiveRole) -> Option<RepoRole> {
    match role {
        EffectiveRole::Read => Some(RepoRole::Read),
        EffectiveRole::Triage => Some(RepoRole::Triage),
        EffectiveRole::Write => Some(RepoRole::Write),
        EffectiveRole::Maintain => Some(RepoRole::Maintain),
        EffectiveRole::Admin => Some(RepoRole::Admin),
        _ => None,
    }
}
pub(crate) fn observed_role(role: &ghinvite_github::RepoRole) -> Result<EffectiveRole, ApiError> {
    use ghinvite_github::RepoRole as Wire;
    Ok(match role {
        Wire::None => EffectiveRole::None,
        Wire::Read => EffectiveRole::Read,
        Wire::Triage => EffectiveRole::Triage,
        Wire::Write => EffectiveRole::Write,
        Wire::Maintain => EffectiveRole::Maintain,
        Wire::Admin => EffectiveRole::Admin,
        Wire::Custom(name) => EffectiveRole::Custom(
            CustomRoleName::new(octoscrub::scrub(name).into_owned()).map_err(|_| {
                ApiError::Decode {
                    what: "custom_role",
                }
            })?,
        ),
        _ => return Err(ApiError::Decode { what: "role" }),
    })
}
fn wire_role(role: RepoRole) -> ghinvite_github::RepoRole {
    use ghinvite_github::RepoRole as Wire;
    match role {
        RepoRole::Read => Wire::Read,
        RepoRole::Triage => Wire::Triage,
        RepoRole::Write => Wire::Write,
        RepoRole::Maintain => Wire::Maintain,
        RepoRole::Admin => Wire::Admin,
    }
}
fn blocked(reason: BlockedReason) -> CreateOutcome {
    CreateOutcome::Blocked { reason }
}
fn throttled(
    now: chrono::DateTime<chrono::Utc>,
    wait: Option<std::time::Duration>,
) -> CreateOutcome {
    CreateOutcome::Throttled {
        reason: None,
        until: now
            + chrono::Duration::from_std(crate::throttle::backoff(wait)).expect("bounded wait"),
    }
}
/// GitHub's requested wait when token creation or the call was throttled.
fn rate_limited(error: &TokenCallError) -> Option<Option<std::time::Duration>> {
    match error {
        TokenCallError::Token(error) => rate_limit(error).map(|(_, wait)| wait),
        TokenCallError::Call(CallError::Api(ApiError::RateLimited { retry_after, .. })) => {
            Some(*retry_after)
        }
        _ => None,
    }
}
fn failed_read(error: &TokenCallError, now: chrono::DateTime<chrono::Utc>) -> CreateOutcome {
    use octotoken::octoauth;
    if let Some(wait) = rate_limited(error) {
        return throttled(now, wait);
    }
    match error {
        TokenCallError::Token(error) => match github_error(error) {
            Some(octoauth::Error::PermissionsNotGranted) => {
                blocked(BlockedReason::InstallationPermissions)
            }
            Some(octoauth::Error::Forbidden {
                reason: octoauth::ForbiddenReason::IpAllowList,
            }) => blocked(BlockedReason::IpAllowList),
            _ => blocked(BlockedReason::MintForbidden),
        },
        TokenCallError::Call(CallError::Api(ApiError::Forbidden {
            reason: ForbiddenReason::IpAllowList,
        })) => blocked(BlockedReason::IpAllowList),
        TokenCallError::Call(CallError::Api(ApiError::Unauthorized)) => {
            blocked(BlockedReason::InstallationUnavailable)
        }
        _ => blocked(BlockedReason::TargetUnavailable),
    }
}

/// S1: verify reach and immutable requester identity, then read role and ALL
/// invitation pages. A successful prefix is never enough to authorize a write.
pub async fn pre_read(
    app: &GithubApp,
    installation: u64,
    command: &CreateCommand,
    now: chrono::DateTime<chrono::Utc>,
) -> PreRead {
    let Ok(repo) = RepositoryIdentity::parse(command.repo_full_name.clone()) else {
        return PreRead::Stopped(CreateOutcome::Failed {
            reason: FailureReason::InvalidTarget,
        });
    };
    // Reads only: a 401 replaces the token and repeats them once.
    let result = app
        .read(installation, &profiles::REPO_READ, |handle| {
            let repo = repo.clone();
            async move {
                let rest = handle.rest();
                let repository = Repository {
                    owner: repo.owner(),
                    name: repo.name(),
                };
                let reached = endpoints::get_repository(&rest, repository).await?;
                if reached.id != command.repo_id {
                    return Ok(PreRead::Stopped(blocked(BlockedReason::TargetUnavailable)));
                }
                let user = endpoints::get_user_by_id(&rest, command.requester_id).await?;
                if user.id != command.requester_id
                    || user.login.is_empty()
                    || user.login.len() > 39
                    || !user
                        .login
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Ok(PreRead::Stopped(blocked(BlockedReason::RequesterIdentity)));
                }
                let named = endpoints::get_user_by_login(&rest, &user.login).await?;
                if named.id != command.requester_id {
                    return Ok(PreRead::Stopped(blocked(BlockedReason::RequesterIdentity)));
                }
                let permission =
                    endpoints::collaborator_permission(&rest, repository, &user.login).await?;
                if permission.user.id != command.requester_id {
                    return Ok(PreRead::Stopped(blocked(BlockedReason::RequesterIdentity)));
                }
                let role = observed_role(&permission.role)?;
                let all = endpoints::list_repo_invitations(&rest, repository).await?;
                let mut matching = all.iter().filter(|i| {
                    !i.expired
                        && i.invitee
                            .as_ref()
                            .is_some_and(|u| u.id == command.requester_id)
                });
                let pending = matching
                    .next()
                    .map(|i| -> Result<_, ApiError> {
                        Ok(PendingInvitation {
                            upstream_id: i.id,
                            timing: ghinvite_core::delivery::InvitationTiming::observed(
                                i.id,
                                i.created_at,
                                chrono::Utc::now(),
                                command.approved_at,
                                false,
                            ),
                            permission: observed_role(&i.permissions)?,
                        })
                    })
                    .transpose()?;
                if matching.next().is_some() {
                    return Ok(PreRead::Stopped(blocked(BlockedReason::TargetUnavailable)));
                }
                Ok::<_, CallError<Infallible>>(PreRead::Ready {
                    login: user.login,
                    role,
                    pending,
                })
            }
        })
        .await;
    result.unwrap_or_else(|error| {
        let limit = budget_limit(&error, ghinvite_core::budget::BucketId::Content, now);
        let outcome = failed_read(&error, now);
        match limit {
            Some(limit) => PreRead::Limited { outcome, limit },
            None => PreRead::Stopped(outcome),
        }
    })
}

/// S4/S5: ensure → re-read → fence → one write. A rejected 401 releases ONLY
/// its own generation and permits one more ensure/claim/call. Any ambiguous
/// result retains the fence; no read absence can authorize replaying a write.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WriteResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<ghinvite_core::delivery::InvitationTiming>,
    pub outcome: CreateOutcome,
    /// Journal positive rejection before S5 releases this exact generation.
    pub rejected_generation: Option<u64>,
    #[serde(default)]
    pub budget: BudgetEvidence,
}

/// Evidence from this execution, never inferred from a recovered create receipt.
/// Missing evidence from older journal entries conservatively retains charges.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BudgetEvidence {
    pub effect: BudgetEffect,
    pub limit: Option<ghinvite_core::budget::RecordLimit>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetEffect {
    #[default]
    Retain,
    ReleaseUnused,
    RefundInvitation,
}

pub async fn write_stage(
    app: &GithubApp,
    installation: u64,
    storage: &dyn DeliveryStorage,
    command: &CreateCommand,
    read: &PreRead,
    now: chrono::DateTime<chrono::Utc>,
) -> WriteResult {
    let mut rejected_generation = None;
    let mut timing = None;
    let mut budget = BudgetEvidence {
        effect: BudgetEffect::ReleaseUnused,
        limit: None,
    };
    let outcome = write_inner(
        app,
        installation,
        storage,
        (command, read),
        now,
        (&mut rejected_generation, &mut budget, &mut timing),
    )
    .await;
    WriteResult {
        timing,
        outcome,
        rejected_generation,
        budget,
    }
}

/// An existing fence permits observation only. Even a sufficient current role
/// cannot establish what happened to the earlier invitation write.
async fn recover_fenced_write(
    app: &GithubApp,
    installation: u64,
    command: &CreateCommand,
    repo: &RepositoryIdentity,
    plan: &Plan,
) -> (
    CreateOutcome,
    Option<ghinvite_core::delivery::InvitationTiming>,
) {
    app.read(installation, &profiles::REPO_READ, |handle| async move {
        let rest = handle.rest();
        let repository = Repository {
            owner: repo.owner(),
            name: repo.name(),
        };
        let invitations = endpoints::list_repo_invitations(&rest, repository).await?;
        let reached = endpoints::get_repository(&rest, repository).await?;
        if reached.id != command.repo_id {
            return Ok((CreateOutcome::OutcomeUnknown, None));
        }
        let mut matching = invitations.iter().filter(|invitation| {
            !invitation.expired
                && invitation
                    .invitee
                    .as_ref()
                    .is_some_and(|user| user.id == command.requester_id)
        });
        let Some(invitation) = matching.next() else {
            return Ok((CreateOutcome::OutcomeUnknown, None));
        };
        if matching.next().is_some()
            || !observed_role(&invitation.permissions)?.satisfies(command.permission.into())
        {
            return Ok((CreateOutcome::OutcomeUnknown, None));
        }
        let outcome = match plan {
            Plan::Put => CreateOutcome::Created {
                upstream_id: invitation.id,
            },
            Plan::Patch { upstream_id } if *upstream_id == invitation.id => {
                CreateOutcome::AlreadyPending {
                    upstream_id: invitation.id,
                    permission: builtin(&observed_role(&invitation.permissions)?)
                        .expect("satisfying role is a grant"),
                }
            }
            _ => CreateOutcome::OutcomeUnknown,
        };
        let timing = ghinvite_core::delivery::InvitationTiming::observed(
            invitation.id,
            invitation.created_at,
            chrono::Utc::now(),
            command.approved_at,
            matches!(outcome, CreateOutcome::Created { .. }),
        );
        Ok::<_, CallError<Infallible>>((outcome, timing))
    })
    .await
    .unwrap_or((CreateOutcome::OutcomeUnknown, None))
}

async fn write_inner(
    app: &GithubApp,
    installation: u64,
    storage: &dyn DeliveryStorage,
    input: (&CreateCommand, &PreRead),
    now: chrono::DateTime<chrono::Utc>,
    evidence: (
        &mut Option<u64>,
        &mut BudgetEvidence,
        &mut Option<ghinvite_core::delivery::InvitationTiming>,
    ),
) -> CreateOutcome {
    let (rejected_generation, budget, timing) = evidence;
    let (command, read) = input;
    let grant = RepoRole::from(command.permission);
    let plan = plan(read, grant);
    if let Plan::Complete(outcome) = plan {
        return outcome;
    }
    let PreRead::Ready {
        login,
        role: before,
        ..
    } = read
    else {
        unreachable!()
    };
    let Ok(repo) = RepositoryIdentity::parse(command.repo_full_name.clone()) else {
        return CreateOutcome::Failed {
            reason: FailureReason::InvalidTarget,
        };
    };
    match storage.delivery_attempt_exists(command).await {
        Ok(true) => {
            budget.effect = BudgetEffect::Retain;
            // Validate the immutable input binding before using its fence.
            return match storage.claim_delivery_attempt(command).await {
                Ok(None) => {
                    let (outcome, observed) =
                        recover_fenced_write(app, installation, command, &repo, &plan).await;
                    *timing = observed;
                    outcome
                }
                _ => CreateOutcome::OutcomeUnknown,
            };
        }
        Ok(false) => {}
        Err(_) => {
            budget.effect = BudgetEffect::Retain;
            return CreateOutcome::OutcomeUnknown;
        }
    }
    let scope = &*profiles::REPO_INVITE;
    if let Err(error) = app.ensure(installation, scope).await {
        return failed_budget_read(TokenCallError::Token(error), now, budget);
    }
    let repo = &repo;
    // Never replayed: a 401 replaces the token and ends this stage.
    let observed = app
        .write_once(installation, scope, |handle| async move {
            let rest = handle.rest();
            endpoints::collaborator_permission(
                &rest,
                Repository {
                    owner: repo.owner(),
                    name: repo.name(),
                },
                login,
            )
            .await
            .map_err(CallError::<Infallible>::Api)
        })
        .await;
    let observed = match observed {
        Ok(value) if value.user.id == command.requester_id => value,
        Ok(_) => return blocked(BlockedReason::RequesterIdentity),
        Err(error) => return failed_budget_read(error, now, budget),
    };
    match observed_role(&observed.role) {
        Ok(role) => {
            if let Some(outcome) = satisfied(&role, grant) {
                return outcome;
            }
        }
        Err(_) => return blocked(BlockedReason::TargetUnavailable),
    }
    for attempt in 0..2 {
        if attempt != 0
            && let Err(error) = app.ensure(installation, scope).await
        {
            return failed_budget_read(TokenCallError::Token(error), now, budget);
        }
        budget.effect = BudgetEffect::Retain;
        let generation = match storage.claim_delivery_attempt(command).await {
            Ok(Some(generation)) => generation,
            // A failed acknowledgement may follow a committed claim.
            Ok(None) => {
                let (outcome, observed) =
                    recover_fenced_write(app, installation, command, repo, &plan).await;
                *timing = observed;
                return outcome;
            }
            Err(_) => return CreateOutcome::OutcomeUnknown,
        };
        let plan = plan.clone();
        // One write per claimed generation, never replayed. A 401 has already
        // replaced the token, so the bounded retry below writes with a new one.
        let result = app
            .write_once(installation, scope, |handle| async move {
                let rest = handle.rest();
                let repository = Repository {
                    owner: repo.owner(),
                    name: repo.name(),
                };
                let result = match plan {
                    Plan::Put => {
                        endpoints::put_collaborator(&rest, repository, login, &wire_role(grant))
                            .await
                            .and_then(|r| {
                                Ok(match r {
                                    CollaboratorWrite::Created {
                                        invitation_id,
                                        created_at,
                                    } => (
                                        CreateOutcome::Created {
                                            upstream_id: invitation_id,
                                        },
                                        created_at.and_then(|at| {
                                            ghinvite_core::delivery::InvitationTiming::observed(
                                                invitation_id,
                                                at,
                                                chrono::Utc::now(),
                                                command.approved_at,
                                                true,
                                            )
                                        }),
                                    ),
                                    CollaboratorWrite::NoContent => (
                                        CreateOutcome::Upgraded {
                                            from: before.clone(),
                                            to: grant,
                                        },
                                        None,
                                    ),
                                    _ => {
                                        return Err(ApiError::Decode {
                                            what: "collaborator_write",
                                        });
                                    }
                                })
                            })
                    }
                    Plan::Patch { upstream_id } => endpoints::update_repo_invitation(
                        &rest,
                        repository,
                        upstream_id,
                        &wire_role(grant),
                    )
                    .await
                    .and_then(|r| {
                        if r.id != upstream_id
                            || r.invitee
                                .as_ref()
                                .is_none_or(|u| u.id != command.requester_id)
                        {
                            return Err(ApiError::Decode {
                                what: "invitation_identity",
                            });
                        }
                        Ok((
                            CreateOutcome::AlreadyPending {
                                upstream_id,
                                permission: grant,
                            },
                            ghinvite_core::delivery::InvitationTiming::observed(
                                upstream_id,
                                r.created_at,
                                chrono::Utc::now(),
                                command.approved_at,
                                false,
                            ),
                        ))
                    }),
                    Plan::Complete(_) => unreachable!(),
                };
                result.map_err(CallError::<Infallible>::Api)
            })
            .await;
        let error = match result {
            Ok((outcome, observed)) => {
                *timing = observed;
                if matches!(outcome, CreateOutcome::Upgraded { .. }) {
                    budget.effect = BudgetEffect::RefundInvitation;
                }
                return outcome;
            }
            Err(error) => error,
        };
        let bucket = match self::plan(read, grant) {
            Plan::Put => ghinvite_core::budget::BucketId::Repo {
                repo_id: command.repo_id,
            },
            _ => ghinvite_core::budget::BucketId::Content,
        };
        budget.limit = budget_limit(&error, bucket, now);
        let (release, outcome, retry) = match &error {
            TokenCallError::Call(CallError::Api(ApiError::Unauthorized)) => (
                true,
                blocked(BlockedReason::InstallationUnavailable),
                attempt == 0,
            ),
            TokenCallError::Call(CallError::Api(ApiError::NotSent)) | TokenCallError::Token(_) => {
                (true, failed_read(&error, now), false)
            }
            TokenCallError::Call(CallError::Api(ApiError::RateLimited { retry_after, .. })) => {
                (true, throttled(now, *retry_after), false)
            }
            TokenCallError::Call(CallError::Api(ApiError::Spammed)) => {
                (true, throttled(now, None), false)
            }
            TokenCallError::Call(CallError::Api(ApiError::Forbidden {
                reason: ForbiddenReason::OutsideCollaboratorsRestricted,
            })) => (
                true,
                blocked(BlockedReason::OutsideCollaboratorsRestricted),
                false,
            ),
            TokenCallError::Call(CallError::Api(ApiError::Forbidden {
                reason: ForbiddenReason::IpAllowList,
            })) => (true, blocked(BlockedReason::IpAllowList), false),
            TokenCallError::Call(CallError::Api(
                ApiError::Forbidden { .. } | ApiError::NotFound,
            )) => (true, blocked(BlockedReason::InstallationUnavailable), false),
            TokenCallError::Call(CallError::Api(ApiError::Validation { codes, .. }))
                if codes.contains(&ValidationCode::OutsideCollaboratorsRestricted) =>
            {
                (
                    true,
                    blocked(BlockedReason::OutsideCollaboratorsRestricted),
                    false,
                )
            }
            TokenCallError::Call(CallError::Api(ApiError::Validation { codes, .. }))
                if codes.contains(&ValidationCode::CannotAssignBelowBase) =>
            {
                (
                    true,
                    CreateOutcome::AlreadySatisfied {
                        effective_role: EffectiveRole::BaseRole,
                    },
                    false,
                )
            }
            TokenCallError::Call(CallError::Api(ApiError::Validation { .. })) => (
                false,
                CreateOutcome::Failed {
                    reason: FailureReason::Rejected { status: 422 },
                },
                false,
            ),
            _ => (false, CreateOutcome::OutcomeUnknown, false),
        };
        if release {
            budget.effect = BudgetEffect::ReleaseUnused;
            if matches!(
                error,
                TokenCallError::Call(CallError::Api(ApiError::Unauthorized))
            ) {
                // Only the bounded 401 retry releases inside S4. Other positive
                // refusals are journaled first so a lost release acknowledgement
                // cannot erase the evidence that GitHub rejected the write.
                if storage
                    .reject_delivery_attempt(command, generation)
                    .await
                    .is_err()
                {
                    *rejected_generation = Some(generation);
                    return outcome;
                }
            } else {
                *rejected_generation = Some(generation);
            }
        }
        if !retry {
            return outcome;
        }
    }
    unreachable!("bounded write attempts return")
}

fn budget_limit(
    error: &TokenCallError,
    spam_bucket: ghinvite_core::budget::BucketId,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<ghinvite_core::budget::RecordLimit> {
    use ghinvite_core::budget::{BucketId, RecordLimit};
    if let Some(retry_after) = rate_limited(error) {
        return Some(RecordLimit::observed(
            BucketId::Content,
            retry_after
                .and_then(|wait| chrono::Duration::from_std(wait).ok())
                .and_then(|wait| now.checked_add_signed(wait)),
            ghinvite_core::budget::LimitReason::RateLimited,
            now,
        ));
    }
    match error {
        TokenCallError::Call(CallError::Api(ApiError::Spammed)) => Some(RecordLimit::observed(
            spam_bucket,
            None,
            ghinvite_core::budget::LimitReason::Spammed,
            now,
        )),
        _ => None,
    }
}
fn failed_budget_read(
    error: TokenCallError,
    now: chrono::DateTime<chrono::Utc>,
    budget: &mut BudgetEvidence,
) -> CreateOutcome {
    budget.limit = budget_limit(&error, ghinvite_core::budget::BucketId::Content, now);
    failed_read(&error, now)
}
