//! External I/O for a separately fenced cancellation. Budget calls belong to
//! the exclusive delivery owner between these stages, never inside this module.
use crate::state::RepositoryGithub;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    Account, RepositoryIdentity, SelectedRepos,
    storage::{DeliveryStorage, delivery_attempts::CancelCommand},
    targets::EffectiveRole,
};
use ghinvite_github::{
    AppSlot, TokenCallError,
    endpoints::{self, Repository},
    error::{ApiError, CallError},
    profiles,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Observation {
    Pending {
        created_at: DateTime<Utc>,
    },
    Accepted {
        role: EffectiveRole,
    },
    Gone,
    Unavailable,
    Limited {
        retry_after: Option<std::time::Duration>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteOutcome {
    Cancelled {
        evidence: CancelEvidence,
    },
    Accepted {
        role: EffectiveRole,
    },
    /// No external effect was attempted, or GitHub positively rejected it.
    NoEffect,
    /// A fence/transport acknowledgement is ambiguous. Never retry the DELETE.
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelEvidence {
    Delete204,
    NotFoundAfterVerifiedReach,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    pub outcome: WriteOutcome,
    pub rejected_generation: Option<u64>,
    pub limit: Option<ghinvite_core::budget::RecordLimit>,
}

pub async fn observe(
    apps: &RepositoryGithub,
    input: &CancelCommand,
    account: &Account,
) -> Observation {
    match observe_inner(apps, input, account).await {
        Ok(observation) => observation,
        Err(error) => match error.rate_limit() {
            Some(limit) => Observation::Limited {
                retry_after: limit.retry_after,
            },
            None => Observation::Unavailable,
        },
    }
}
async fn observe_inner(
    apps: &RepositoryGithub,
    input: &CancelCommand,
    account: &Account,
) -> crate::Result<Observation> {
    let command = &input.create;
    if account.account_id != command.account_id
        || account.uninstalled_at.is_some()
        || input.upstream_id == 0
        || matches!(&account.selected_repos, SelectedRepos::Subset(ids) if !ids.contains(&command.repo_id))
    {
        return Ok(Observation::Unavailable);
    }
    let identity = RepositoryIdentity::parse(command.repo_full_name.clone())
        .map_err(|_| crate::HandlerError::Invariant("invalid repository identity".into()))?;
    let app = apps
        .apps
        .app(AppSlot::Repo)
        .map_err(|_| crate::HandlerError::Invariant("repository app unavailable".into()))?;
    let app_rest = app.app_rest().map_err(octotoken::TokenError::from)?;
    let installation =
        endpoints::get_installation(&app_rest.rest(), account.installation_id).await?;
    if installation.id != account.installation_id
        || installation.account.id != account.account_id
        || app.id().get() != installation.app_id
        || installation.suspended_at.is_some()
    {
        return Ok(Observation::Unavailable);
    }
    let identity = &identity;
    // Reads only: a 401 replaces the token and repeats them once.
    app.read(
        account.installation_id,
        &profiles::REPO_READ,
        |handle| async move {
            let rest = handle.rest();
            let repo = Repository {
                owner: identity.owner(),
                name: identity.name(),
            };
            let current = endpoints::authority::repository_identity(&rest, repo).await?;
            if current.id != command.repo_id
                || current.owner.id != command.account_id
                || !current
                    .full_name
                    .eq_ignore_ascii_case(&command.repo_full_name)
            {
                return Ok(Observation::Unavailable);
            }
            let invitations = endpoints::list_repo_invitations(&rest, repo).await?;
            if let Some(invitation) = invitations.iter().find(|i| i.id == input.upstream_id) {
                if invitation.expired
                    || invitation
                        .invitee
                        .as_ref()
                        .is_none_or(|u| u.id != command.requester_id)
                {
                    return Ok(Observation::Unavailable);
                }
                return Ok(Observation::Pending {
                    created_at: invitation.created_at,
                });
            }
            let user = endpoints::get_user_by_id(&rest, command.requester_id).await?;
            if user.id != command.requester_id {
                return Ok(Observation::Unavailable);
            }
            let role = endpoints::collaborator_permission(&rest, repo, &user.login).await?;
            if role.user.id != command.requester_id {
                return Ok(Observation::Unavailable);
            }
            let role = crate::repository_protocol::observed_role(&role.role)?;
            Ok::<_, CallError<Infallible>>(if role.satisfies(command.permission.into()) {
                Observation::Accepted { role }
            } else {
                Observation::Gone
            })
        },
    )
    .await
    .map_err(Into::into)
}

/// Token acquisition precedes a fresh read and the fence. Retry-on-401 is
/// explicit and releases the rejected generation before the second claim.
pub async fn write(
    apps: &RepositoryGithub,
    storage: &dyn DeliveryStorage,
    input: &CancelCommand,
    account: &Account,
    now: DateTime<Utc>,
) -> WriteResult {
    // A run may restart after the external effect but before its result was
    // journaled. Do not reinterpret fresh absence as an unused budget slot.
    if storage
        .cancellation_attempt_exists(input)
        .await
        .unwrap_or(true)
    {
        return WriteResult {
            outcome: WriteOutcome::Unknown,
            rejected_generation: None,
            limit: None,
        };
    }
    let mut rejected_generation = None;
    let mut limit = None;
    let scope = &*profiles::REPO_INVITE;
    let outcome = async {
        let Ok(identity) = RepositoryIdentity::parse(input.create.repo_full_name.clone()) else {
            return WriteOutcome::NoEffect;
        };
        // Configured with the runtime; absent, no token can be had.
        let Ok(app) = apps.apps.app(AppSlot::Repo) else {
            return WriteOutcome::NoEffect;
        };
        for attempt in 0..2 {
            if let Err(error) = app.ensure(account.installation_id, scope).await {
                if let Some((_, retry_after)) = ghinvite_github::apps::rate_limit(&error) {
                    limit = Some(content_limit(now, retry_after));
                }
                return WriteOutcome::NoEffect;
            }
            match observe(apps, input, account).await {
                Observation::Pending { .. } => {}
                Observation::Accepted { role } => return WriteOutcome::Accepted { role },
                Observation::Limited { retry_after } => {
                    limit = Some(content_limit(now, retry_after));
                    return WriteOutcome::NoEffect;
                }
                _ => return WriteOutcome::NoEffect,
            }
            let generation = match storage.claim_cancellation_attempt(input).await {
                Ok(Some(generation)) => generation,
                _ => return WriteOutcome::Unknown,
            };
            let repo = Repository {
                owner: identity.owner(),
                name: identity.name(),
            };
            // Never replayed; a 401 has replaced the token before the retry.
            let result = app
                .write_once(account.installation_id, scope, |handle| async move {
                    endpoints::delete_repo_invitation(&handle.rest(), repo, input.upstream_id)
                        .await
                        .map_err(CallError::<Infallible>::Api)
                })
                .await;
            match result {
                Ok(()) => {
                    return WriteOutcome::Cancelled {
                        evidence: CancelEvidence::Delete204,
                    };
                }
                Err(TokenCallError::Call(CallError::Api(ApiError::Unauthorized))) => {
                    rejected_generation = Some(generation);
                    if storage
                        .reject_cancellation_attempt(input, generation)
                        .await
                        .is_err()
                    {
                        return WriteOutcome::NoEffect;
                    }
                    if attempt == 0 {
                        continue;
                    }
                    return WriteOutcome::NoEffect;
                }
                Err(TokenCallError::Call(CallError::Api(ApiError::NotFound))) => {
                    // An observed 404 is interpreted only after a fresh reach
                    // and acceptance check. Lost HTTP results never enter here.
                    return match observe(apps, input, account).await {
                        Observation::Accepted { role } => WriteOutcome::Accepted { role },
                        Observation::Gone => WriteOutcome::Cancelled {
                            evidence: CancelEvidence::NotFoundAfterVerifiedReach,
                        },
                        Observation::Limited { retry_after } => {
                            limit = Some(content_limit(now, retry_after));
                            WriteOutcome::Unknown
                        }
                        _ => WriteOutcome::Unknown,
                    };
                }
                Err(TokenCallError::Call(CallError::Api(ApiError::RateLimited {
                    retry_after,
                    ..
                }))) => {
                    limit = Some(content_limit(now, retry_after));
                    rejected_generation = Some(generation);
                    return WriteOutcome::NoEffect;
                }
                Err(TokenCallError::Call(CallError::Api(ApiError::Spammed))) => {
                    let mut observed = content_limit(now, None);
                    observed
                        .observation
                        .as_mut()
                        .expect("observed limit")
                        .reason = ghinvite_core::budget::LimitReason::Spammed;
                    limit = Some(observed);
                    rejected_generation = Some(generation);
                    return WriteOutcome::NoEffect;
                }
                Err(TokenCallError::Call(CallError::Api(
                    ApiError::NotSent | ApiError::Forbidden { .. } | ApiError::Validation { .. },
                ))) => {
                    rejected_generation = Some(generation);
                    return WriteOutcome::NoEffect;
                }
                _ => return WriteOutcome::Unknown,
            }
        }
        WriteOutcome::NoEffect
    }
    .await;
    WriteResult {
        outcome,
        rejected_generation,
        limit,
    }
}

pub(crate) fn content_limit(
    now: DateTime<Utc>,
    wait: Option<std::time::Duration>,
) -> ghinvite_core::budget::RecordLimit {
    // Anchor a missing/invalid retry deadline once so completion replay cannot
    // continually extend a closure by reinterpreting None at a later time.
    let wait = wait
        .and_then(|w| chrono::Duration::from_std(w).ok())
        .unwrap_or_else(|| chrono::Duration::days(1));
    ghinvite_core::budget::RecordLimit::observed(
        ghinvite_core::budget::BucketId::Content,
        now.checked_add_signed(wait),
        ghinvite_core::budget::LimitReason::RateLimited,
        now,
    )
}
