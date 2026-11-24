//! Restate handler services for ghinvite.
//!
//! Restate is the authority for every durable state change —
//! [`admission`] and [`request_owner`] for invitation requests,
//! [`delivery`] and [`settlement`] for GitHub invitations,
//! [`availability`] for installations — and [`projection`] keeps the
//! queryable SQL records.
//!
//! A handler that owns a storage effect journals it through `ctx.run`,
//! delegating to a function over [`state::AppState`] that a unit test can call
//! without a Restate runtime. A handler that only routes works on its
//! `ObjectContext` directly — [`installation::Installation`] resolves an account
//! and calls the account object, and keeps no state of its own beyond that
//! binding. Tests that need the real runtime go against the local
//! `restate-server` (compose.yaml) behind the `integration` feature; see
//! `scripts/test-restate.sh`.

pub mod access_grant;
mod account_budget;
pub mod admission;
pub mod availability;
pub mod bulk;
pub mod cancellation_protocol;
mod clock;
pub mod delivery;
mod delivery_budget;
pub mod delivery_cancellation;
pub mod delivery_control;
#[doc(hidden)]
pub mod delivery_run_fixture;
pub mod email_delivery;
pub mod endpoint_identity;
pub mod error;
pub mod github_invitation;
pub mod installation;
pub mod invite_budget;
mod link_access_removal;
mod link_control;
mod link_revocation;
mod operational;
pub mod org_delivery;
pub mod org_eligibility;
pub mod org_reconcile;
pub mod projection;
pub mod reconcile;
mod repository_audit;
pub mod repository_protocol;
mod request_control;
mod request_jwt;
pub mod request_owner;
pub mod settlement;
pub mod settlement_observation;
pub mod settlement_policy;
pub mod state;
pub mod sweep_schedule;
pub mod throttle;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod test_github;

pub use error::{HandlerError, Result};
pub use state::{AppState, RepositoryGithub, WorkflowStorage};

use restate_sdk::endpoint::Endpoint;

/// Build the Restate endpoint serving every ghinvite service.
///
/// `identity_key` is the Restate Cloud identity public key
/// (`publickeyv1_...`). When `Some`, the endpoint will reject any request not
/// signed by Restate Cloud. Pass `None` only in local dev where Restate runs
/// without identity signing (plain `docker compose up`).
///
/// Returns `Err` if `identity_key` is `Some` but is not a valid
/// `publickeyv1_`-prefixed Ed25519 key — callers should treat this as a fatal
/// startup error.
///
/// Deployment adapters use [`build_endpoint_with_identity`] with a validated
/// `RESTATE_IDENTITY_KEYS` list; this convenience entry point is for local tests.
pub fn build_endpoint(
    state: AppState,
    projection: std::sync::Arc<dyn ghinvite_core::storage::projection::ProjectionStorage>,
    identity_key: Option<&str>,
) -> std::result::Result<Endpoint, String> {
    let identity = match identity_key {
        Some(value) => {
            endpoint_identity::EndpointIdentity::required(Some(value)).map_err(|e| e.to_string())?
        }
        None => endpoint_identity::EndpointIdentity::local_unsigned(),
    };
    build_endpoint_with_identity(state, projection, &identity).map_err(|e| e.to_string())
}

/// Build all services with an explicitly chosen, validated identity policy.
pub fn build_endpoint_with_identity(
    state: AppState,
    projection: std::sync::Arc<dyn ghinvite_core::storage::projection::ProjectionStorage>,
    identity: &endpoint_identity::EndpointIdentity,
) -> std::result::Result<Endpoint, endpoint_identity::IdentityConfigError> {
    let builder = Endpoint::builder()
        .bind(installation::Installation {
            state: state.clone(),
        })
        .bind(reconcile::Reconcile {
            state: state.clone(),
        })
        .bind(availability::AccountInstallation {
            state: state.clone(),
        })
        .bind(availability::InstallationProjection {
            state: state.clone(),
        });
    let organization_enabled = state.organization_delivery.is_some();
    let builder = admission::bind_with_all_capabilities(
        builder,
        state.cancellation_enabled && state.invite_budget.is_some(),
        organization_enabled,
        state.access_removal_enabled,
    );
    let builder = projection::bind(builder, projection);
    #[cfg(feature = "integration")]
    let builder = if let Some(faults) = &state.request_faults {
        request_owner::bind_with_feature_faults(
            builder,
            organization_enabled,
            state.reissue_enabled,
            state.bulk.is_some(),
            faults.clone(),
        )
    } else {
        request_owner::bind_with_features(
            builder,
            organization_enabled,
            state.reissue_enabled,
            state.bulk.is_some(),
        )
    };
    #[cfg(not(feature = "integration"))]
    let builder = request_owner::bind_with_features(
        builder,
        organization_enabled,
        state.reissue_enabled,
        state.bulk.is_some(),
    );
    let builder = if let Some(storage) = &state.bulk {
        bulk::bind(builder, state.clone(), storage.clone())
    } else {
        builder
    };
    let builder = if let Some(storage) = &state.invite_budget {
        invite_budget::bind_pre_activation(builder, storage.clone(), Default::default())
    } else {
        builder
    };
    let builder = if let Some(storage) = &state.organization_delivery {
        org_delivery::bind(builder, state.clone(), storage.clone()).bind(
            org_eligibility::OrganizationEligibility {
                state: state.clone(),
            },
        )
    } else {
        builder
    };
    let builder = if let Some(storage) = &state.access_grants {
        access_grant::bind(builder, state.clone(), storage.clone())
    } else {
        builder
    };
    let builder = if state.organization_delivery.is_some() || state.email_deliveries.is_some() {
        builder.bind(org_reconcile::OrganizationReconcile {
            state: state.clone(),
        })
    } else {
        builder
    };
    let builder = if let Some(dependencies) = &state.email_deliveries {
        email_delivery::bind(builder, state.clone(), dependencies.clone())
    } else {
        builder
    };
    let builder = delivery::bind(builder, state);
    let builder = identity.apply(builder)?;
    Ok(builder.build())
}

mod delivery_batch_cancel;
mod delivery_reissue;
mod request_batch_cancel;

mod request_reissue;
