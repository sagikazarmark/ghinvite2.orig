//! Restate handler services for ghinvite. Which services own durable state
//! depends on which of the two endpoints is bound.
//!
//! [`build_endpoint`] is the pre-cutover default (`GHINVITE_ADMISSION_MODE`
//! unset or `legacy`). It binds [`invitation_link::InvitationLink`],
//! [`invitation_request::InvitationRequest`],
//! [`github_invitation::GithubInvitation`] and [`reconcile::Reconcile`] as their
//! real implementations, so those still own the state they always did.
//! Installations are the exception: they were cut over ahead of the rest, so
//! both endpoints route them through [`availability::AccountInstallationV1`] and
//! [`availability::InstallationProjectionV1`].
//!
//! [`build_cutover_endpoint`] (`GHINVITE_ADMISSION_MODE=authoritative`) is the
//! shape after cutover. There the `_v1` services own every durable state change
//! — [`admission_v1`] and [`request_lifecycle_v1`] for invitation requests,
//! [`delivery_v1`] and [`settlement_v1`] for GitHub invitations,
//! [`projection_v1`] for the queryable records — and the four writers above stay
//! bound only for deployments pinned before the cutover, forwarding to the
//! current authority or answering 410 (see `obsolete_writers`).
//!
//! A handler that owns a storage effect journals it through `ctx.run`,
//! delegating to a function over [`state::AppState`] that a unit test can call
//! without a Restate runtime. A handler that only routes works on its
//! `ObjectContext` directly — [`installation::Installation`] resolves an account
//! and calls the account object, and keeps no state of its own beyond that
//! binding. Tests that need the real runtime go against the local
//! `restate-server` (compose.yaml) behind the `integration` feature; see
//! `scripts/test-restate.sh`.

// SDK 0.12 retains the trait-based service API. Preserve these deployed contracts
// during the clock compatibility upgrade; migrating macro style is separate work.
#[allow(deprecated)]
pub mod admission_v1;
pub mod audit;
#[allow(deprecated)]
pub mod availability;
#[allow(deprecated)]
pub mod delivery_v1;
pub mod error;
#[allow(deprecated)]
pub mod github_invitation;
#[allow(deprecated)]
pub mod installation;
pub mod invitation_context;
#[allow(deprecated)]
pub mod invitation_link;
#[allow(deprecated)]
pub mod invitation_request;
pub mod migration_v1;
mod obsolete_writers;
pub mod projection_v1;
#[allow(deprecated)]
pub mod reconcile;
pub mod request_lifecycle_v1;
pub mod settlement_v1;
pub mod state;
pub mod throttle;

#[cfg(test)]
pub(crate) mod test_support;

// Re-exports filled in as each module gains its public types:
pub use error::{HandlerError, Result};
pub use state::AppState;

use restate_sdk::endpoint::Endpoint;

/// Native cutover endpoint, registered only after checkpoint and old endpoint
/// isolation. Projection ownership and ingress reopening are operator gates.
pub fn build_cutover_endpoint(
    state: AppState,
    projection: std::sync::Arc<dyn ghinvite_core::storage::projection::ProjectionStorage>,
    identity_key: Option<&str>,
) -> std::result::Result<Endpoint, String> {
    use github_invitation::GithubInvitation as _;
    use installation::Installation as _;
    use reconcile::Reconcile as _;
    let builder = Endpoint::builder()
        .bind(invitation_link::InvitationLink::serve(
            obsolete_writers::Obsolete,
        ))
        .bind(invitation_request::InvitationRequest::serve(
            obsolete_writers::Obsolete,
        ))
        .bind(obsolete_writers::GithubLifecycle(state.clone()).serve())
        .bind(
            installation::InstallationImpl {
                state: state.clone(),
            }
            .serve(),
        )
        .bind(obsolete_writers::ReconcileLifecycle(state.clone()).serve());
    let builder = admission_v1::bind(builder);
    let builder = builder.bind(availability::AccountInstallationV1::serve(
        availability::AccountInstallationV1Impl {
            state: state.clone(),
        },
    ));
    let builder = builder.bind(availability::InstallationProjectionV1::serve(
        availability::InstallationProjectionV1Impl {
            state: state.clone(),
        },
    ));
    let builder = projection_v1::bind(builder, projection);
    let builder = request_lifecycle_v1::bind(builder);
    let mut builder = delivery_v1::bind(builder, state);
    if let Some(key) = identity_key {
        builder = builder.identity_key(key).map_err(|e| e.to_string())?;
    }
    Ok(builder.build())
}

/// Build the pre-cutover endpoint: the legacy writers still serving their own
/// state alongside the installation services that replaced theirs.
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
/// **Production:** always pass `Some` — set `RESTATE_IDENTITY_KEY` via
/// `wrangler secret put RESTATE_IDENTITY_KEY --config wrangler/restate-svc.toml`.
pub fn build_endpoint(
    state: AppState,
    identity_key: Option<&str>,
) -> std::result::Result<Endpoint, String> {
    use github_invitation::GithubInvitation as _;
    use installation::Installation as _;
    use invitation_link::InvitationLink as _;
    use invitation_request::InvitationRequest as _;
    use reconcile::Reconcile as _;

    let mut builder = Endpoint::builder()
        .bind(
            installation::InstallationImpl {
                state: state.clone(),
            }
            .serve(),
        )
        .bind(
            invitation_link::InvitationLinkImpl {
                state: state.clone(),
            }
            .serve(),
        )
        .bind(
            invitation_request::InvitationRequestImpl {
                state: state.clone(),
            }
            .serve(),
        )
        .bind(
            github_invitation::GithubInvitationImpl {
                state: state.clone(),
            }
            .serve(),
        )
        .bind(availability::AccountInstallationV1::serve(
            availability::AccountInstallationV1Impl {
                state: state.clone(),
            },
        ))
        .bind(availability::InstallationProjectionV1::serve(
            availability::InstallationProjectionV1Impl {
                state: state.clone(),
            },
        ))
        .bind(reconcile::ReconcileImpl { state }.serve());

    if let Some(key) = identity_key {
        builder = builder
            .identity_key(key)
            .map_err(|e| format!("RESTATE_IDENTITY_KEY is invalid: {e}"))?;
    }

    Ok(builder.build())
}
