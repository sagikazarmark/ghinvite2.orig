//! HTTP and vault effects stay inside a run; every returned value is secret-free.
use crate::RepositoryGithub;
use chrono::{DateTime, Duration, Utc};
use ghinvite_core::{
    Account, AccountType,
    budget::{BucketId, LimitReason, RecordLimit},
    email_delivery::*,
    storage::{email_delivery::EmailDeliveryStorage, email_vault::EmailVaultBinding},
};
use ghinvite_email_vault::EmailVault;
use ghinvite_github::{
    AppSlot, GithubApp, TokenCallError,
    endpoints::{self, organization as api},
    error::{ApiError, CallError, ForbiddenReason},
    profiles::{ORG_INVITE, ORG_READ},
};
use octotoken::{TokenError, octoauth};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Completion {
    pub outcome: EmailCreateOutcome,
    pub unused: bool,
    pub rejected_generation: Option<u64>,
    pub limit: Option<RecordLimit>,
    pub recheck: bool,
}
fn blocked(reason: EmailBlockedReason) -> Completion {
    Completion {
        outcome: EmailCreateOutcome::Blocked { reason },
        unused: true,
        rejected_generation: None,
        limit: None,
        recheck: false,
    }
}
fn unknown() -> Completion {
    Completion {
        outcome: EmailCreateOutcome::OutcomeUnknown,
        unused: false,
        rejected_generation: None,
        limit: None,
        recheck: false,
    }
}
fn limited(now: DateTime<Utc>, wait: Option<std::time::Duration>, spam: bool) -> Completion {
    let until = now
        + wait
            .and_then(|v| Duration::from_std(v).ok())
            .unwrap_or_else(|| Duration::days(1));
    Completion {
        outcome: EmailCreateOutcome::Throttled {
            until,
            reason: None,
        },
        unused: true,
        rejected_generation: None,
        limit: Some(RecordLimit::observed(
            if spam {
                BucketId::Org
            } else {
                BucketId::Content
            },
            Some(until),
            if spam {
                LimitReason::Spammed
            } else {
                LimitReason::RateLimited
            },
            now,
        )),
        recheck: false,
    }
}
fn mint(error: TokenError, now: DateTime<Utc>) -> Completion {
    match ghinvite_github::apps::github_error(&error) {
        Some(octoauth::Error::RateLimited { retry_after, .. }) => limited(now, *retry_after, false),
        Some(octoauth::Error::PermissionsNotGranted) => {
            let mut r = blocked(EmailBlockedReason::InstallationPermissions);
            r.recheck = true;
            r
        }
        Some(octoauth::Error::Forbidden { .. }) => blocked(EmailBlockedReason::MintForbidden),
        _ => blocked(EmailBlockedReason::InstallationUnavailable),
    }
}
fn read_error(error: crate::HandlerError, now: DateTime<Utc>) -> Completion {
    match error {
        crate::HandlerError::Token(error) => mint(error, now),
        crate::HandlerError::GithubApi(ApiError::RateLimited { retry_after, .. }) => {
            limited(now, retry_after, false)
        }
        crate::HandlerError::GithubApi(ApiError::Forbidden {
            reason: ForbiddenReason::IpAllowList,
        }) => blocked(EmailBlockedReason::IpAllowList),
        crate::HandlerError::GithubApi(ApiError::Decode {
            what: "email_team_managed",
        }) => blocked(EmailBlockedReason::IdpSyncedTeam),
        crate::HandlerError::GithubApi(ApiError::Decode {
            what: "email_no_seat",
        }) => blocked(EmailBlockedReason::NoSeat),
        _ => blocked(EmailBlockedReason::TargetUnavailable),
    }
}
fn invalid(what: &'static str) -> ApiError {
    ApiError::Decode { what }
}
async fn verify_installation(
    apps: &RepositoryGithub,
    c: &EmailCreateCommand,
    a: &Account,
) -> crate::Result<GithubApp> {
    if a.account_id != c.account_id
        || a.account_type != AccountType::Organization
        || a.uninstalled_at.is_some()
    {
        return Err(invalid("email_installation").into());
    }
    let app = apps
        .apps
        .app(AppSlot::Org)
        .map_err(|_| invalid("email_app"))?;
    let credential = app.app_rest().map_err(TokenError::from)?;
    let actual = endpoints::get_installation(&credential.rest(), a.installation_id).await?;
    if actual.id != a.installation_id
        || actual.account.id != c.account_id
        || actual.suspended_at.is_some()
        || app.id().get() != actual.app_id
    {
        return Err(invalid("email_installation").into());
    }
    Ok(app)
}
/// No numeric-user lookup, membership lookup, or address matching is permitted.
async fn preflight(
    apps: &RepositoryGithub,
    c: &EmailCreateCommand,
    a: &Account,
) -> crate::Result<String> {
    let app = verify_installation(apps, c, a).await?;
    app.read(a.installation_id, &ORG_READ, |handle| async move {
        let rest = handle.rest();
        let org = api::get_organization(&rest, &a.account_login).await?;
        if org.id != c.organization.org_id || !org.login.eq_ignore_ascii_case(&a.account_login) {
            return Err(CallError::Api(invalid("email_org")));
        }
        if org
            .plan
            .as_ref()
            .and_then(api::OrganizationPlan::paid_seats)
            .is_some_and(|(n, used)| used >= n)
        {
            return Err(CallError::Api(invalid("email_no_seat")));
        }
        for target in &c.teams {
            let team = api::get_team(&rest, &org.login, &target.team_slug).await?;
            if team.id != target.team_id {
                return Err(CallError::Api(invalid("email_team")));
            }
            if team.externally_managed {
                return Err(CallError::Api(invalid("email_team_managed")));
            }
        }
        Ok::<_, CallError<Infallible>>(org.login)
    })
    .await
    .map_err(Into::into)
}
pub async fn write(
    apps: &RepositoryGithub,
    storage: &dyn EmailDeliveryStorage,
    vault: &EmailVault,
    input: &EmailAttempt,
    account: &Account,
    now: DateTime<Utc>,
) -> Result<Completion, ghinvite_core::storage::Error> {
    let c = &input.create;
    c.validate()
        .map_err(|_| ghinvite_core::storage::email_delivery::invalid())?;
    // The fence is checked before touching recovery data or making any HTTP call.
    if storage.email_attempt_exists(input).await? {
        return Ok(unknown());
    }
    let binding = EmailVaultBinding {
        email_ref: c.email_ref,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
    };
    // The vault's `Address` is in the normal form `email_invitation::create`
    // re-validates, so `create` never refuses it.
    let address = match vault.load_active(&binding, now.timestamp()).await {
        Ok(a) => a,
        Err(_) => return Ok(blocked(EmailBlockedReason::EmailRecoveryUnavailable)),
    };
    let app = match apps.apps.app(AppSlot::Org) {
        Ok(app) => app,
        Err(_) => return Ok(blocked(EmailBlockedReason::InstallationUnavailable)),
    };
    for retry in 0..2 {
        let org = match preflight(apps, c, account).await {
            Ok(org) => org,
            Err(e) => return Ok(read_error(e, now)),
        };
        if let Err(e) = app.ensure(account.installation_id, &ORG_INVITE).await {
            return Ok(mint(e, now));
        }
        let Some(generation) = storage.claim_email_attempt(input).await? else {
            return Ok(unknown());
        };
        let address = &address;
        let result = app
            .write_once(account.installation_id, &ORG_INVITE, |handle| async move {
                ghinvite_github::email_invitation::create(
                    &handle.rest(),
                    &org,
                    address.as_str(),
                    &c.teams.iter().map(|t| t.team_id).collect::<Vec<_>>(),
                )
                .await
                .map_err(CallError::<Infallible>::Api)
            })
            .await;
        match result {
            Ok(receipt)
                if receipt.id <= ghinvite_core::storage::integer::MAX_EXACT
                    && receipt.created_at.timestamp() >= c.approved_at.timestamp() =>
            {
                return Ok(Completion {
                    outcome: EmailCreateOutcome::Created {
                        upstream_id: receipt.id,
                        created_at: receipt.created_at,
                    },
                    unused: false,
                    rejected_generation: None,
                    limit: None,
                    recheck: false,
                });
            }
            Err(TokenCallError::Call(CallError::Api(ApiError::Unauthorized))) => {
                // A positive401 excludes a write. Release must be acknowledged before retry.
                if storage
                    .reject_email_attempt(input, generation)
                    .await
                    .is_err()
                {
                    let mut r = blocked(EmailBlockedReason::InstallationUnavailable);
                    r.rejected_generation = Some(generation);
                    r.recheck = true;
                    return Ok(r);
                }
                if retry == 0 {
                    continue;
                }
                let mut r = blocked(EmailBlockedReason::InstallationUnavailable);
                r.recheck = true;
                return Ok(r);
            }
            Err(TokenCallError::Call(CallError::Api(error))) => {
                let mut result = match error {
                    ApiError::RateLimited { retry_after, .. } => limited(now, retry_after, false),
                    ApiError::Spammed => limited(now, None, true),
                    ApiError::Forbidden { reason } => blocked(match reason {
                        ForbiddenReason::IpAllowList => EmailBlockedReason::IpAllowList,
                        _ => EmailBlockedReason::InstallationPermissions,
                    }),
                    ApiError::NotSent => blocked(EmailBlockedReason::TargetUnavailable),
                    ApiError::Validation { .. }
                    | ApiError::Conflict
                    | ApiError::NotAllowed
                    | ApiError::NotFound
                    | ApiError::Gone => Completion {
                        outcome: EmailCreateOutcome::Failed {
                            reason: ghinvite_core::delivery::FailureReason::Rejected {
                                status: match error {
                                    ApiError::NotFound => 404,
                                    ApiError::Gone => 410,
                                    ApiError::Conflict => 409,
                                    ApiError::NotAllowed => 405,
                                    _ => 422,
                                },
                            },
                        },
                        unused: true,
                        rejected_generation: None,
                        limit: None,
                        recheck: false,
                    },
                    _ => return Ok(unknown()),
                };
                result.rejected_generation = Some(generation);
                return Ok(result);
            }
            // Broker failure after claim, decode failure,5xx,timeout: never repeatPOST.
            _ => return Ok(unknown()),
        }
    }
    unreachable!("bounded401 loop returns")
}
