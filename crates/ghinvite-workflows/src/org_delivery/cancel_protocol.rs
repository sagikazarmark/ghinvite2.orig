//! Organization invitation cancellation I/O. No budget or Restate RPCs here.
use super::*;
use ghinvite_github::{
    AppSlot, TokenCallError,
    apps::rate_limit,
    endpoints::organization as api,
    error::{ApiError, CallError},
    profiles::ORG_INVITE,
};
use std::convert::Infallible;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Observation {
    Pending { org_login: String },
    Accepted,
    Absent,
    Unavailable,
    Limited(ghinvite_core::budget::RecordLimit),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Outcome {
    Cancelled,
    Accepted,
    NoEffect,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Result {
    pub outcome: Outcome,
    pub unused: bool,
    pub rejected_generation: Option<u64>,
    pub limit: Option<ghinvite_core::budget::RecordLimit>,
}
pub(super) async fn observe(
    apps: &crate::RepositoryGithub,
    input: &OrganizationAttempt,
    account: &ghinvite_core::Account,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Observation {
    let OrganizationWrite::Cancel { upstream_id, .. } = input.action else {
        return Observation::Unavailable;
    };
    match protocol::cancellation_read(apps, &input.create, account, now).await {
        protocol::PreRead::Stopped {
            limit: Some(limit), ..
        } => Observation::Limited(limit),
        protocol::PreRead::Stopped { .. } => Observation::Unavailable,
        protocol::PreRead::Ready(read) => match read.organization {
            protocol::OrganizationRead::Active { .. } => Observation::Accepted,
            protocol::OrganizationRead::Pending {
                upstream_id: found,
                created_at: at,
                ..
            } if found == upstream_id && at == created_at && at <= now => Observation::Pending {
                org_login: read.org_login,
            },
            protocol::OrganizationRead::Missing => Observation::Absent,
            _ => Observation::Unavailable,
        },
    }
}
pub(super) async fn write(
    apps: &crate::RepositoryGithub,
    storage: &dyn OrgDeliveryStorage,
    input: &OrganizationAttempt,
    account: &ghinvite_core::Account,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result {
    let mut result = Result {
        outcome: Outcome::Unknown,
        unused: false,
        rejected_generation: None,
        limit: None,
    };
    if storage
        .organization_attempt_exists(input)
        .await
        .unwrap_or(true)
    {
        return result;
    }
    let Ok(app) = apps.apps.app(AppSlot::Org) else {
        return Result {
            outcome: Outcome::NoEffect,
            unused: true,
            ..result
        };
    };
    let OrganizationWrite::Cancel { upstream_id, .. } = input.action else {
        return result;
    };
    for retry in 0..2 {
        if let Err(error) = app.ensure(account.installation_id, &ORG_INVITE).await {
            result.unused = true;
            result.outcome = Outcome::NoEffect;
            if let Some((_, retry_after)) = rate_limit(&error) {
                result.limit = Some(protocol::content_limit(now, retry_after));
            }
            return result;
        }
        let org_login = match observe(apps, input, account, created_at, now).await {
            Observation::Pending { org_login } => org_login,
            Observation::Accepted => {
                return Result {
                    outcome: Outcome::Accepted,
                    unused: true,
                    ..result
                };
            }
            Observation::Limited(limit) => {
                return Result {
                    outcome: Outcome::NoEffect,
                    unused: true,
                    limit: Some(limit),
                    ..result
                };
            }
            _ => {
                return Result {
                    outcome: Outcome::NoEffect,
                    unused: true,
                    ..result
                };
            }
        };
        let generation = match storage.claim_organization_attempt(input).await {
            Ok(Some(generation)) => generation,
            _ => return result,
        };
        let response = app
            .write_once(account.installation_id, &ORG_INVITE, |handle| {
                let org_login = &org_login;
                async move {
                    api::cancel_invitation(&handle.rest(), org_login, upstream_id)
                        .await
                        .map_err(CallError::<Infallible>::Api)
                }
            })
            .await;
        match response {
            Ok(()) => {
                return Result {
                    outcome: Outcome::Cancelled,
                    ..result
                };
            }
            Err(TokenCallError::Call(CallError::Api(ApiError::Unauthorized))) => {
                result.rejected_generation = Some(generation);
                if storage
                    .reject_organization_attempt(input, generation)
                    .await
                    .is_err()
                {
                    return Result {
                        outcome: Outcome::NoEffect,
                        unused: true,
                        ..result
                    };
                }
                if retry == 0 {
                    continue;
                }
                return Result {
                    outcome: Outcome::NoEffect,
                    unused: true,
                    ..result
                };
            }
            Err(TokenCallError::Call(CallError::Api(error))) => {
                if matches!(
                    error,
                    ApiError::Forbidden { .. }
                        | ApiError::Validation { .. }
                        | ApiError::Spammed
                        | ApiError::RateLimited { .. }
                        | ApiError::NotSent
                ) {
                    result.rejected_generation = Some(generation);
                    result.unused = true;
                    result.outcome = Outcome::NoEffect;
                    result.limit = match error {
                        ApiError::RateLimited { retry_after, .. } => {
                            Some(protocol::content_limit(now, retry_after))
                        }
                        ApiError::Spammed => {
                            let mut limit = protocol::content_limit(now, None);
                            limit.observation.as_mut().expect("observed limit").reason =
                                ghinvite_core::budget::LimitReason::Spammed;
                            Some(limit)
                        }
                        _ => None,
                    };
                }
                // A missing invitation after the read can mean acceptance or
                // another actor's cancellation. Neither 404 nor a lost ACK is
                // evidence that our DELETE succeeded.
                return result;
            }
            Err(TokenCallError::Token(error)) => {
                result.rejected_generation = Some(generation);
                result.unused = true;
                result.outcome = Outcome::NoEffect;
                if let Some((_, retry_after)) = rate_limit(&error) {
                    result.limit = Some(protocol::content_limit(now, retry_after));
                }
                return result;
            }
        }
    }
    result
}
