//! Restate handler services for ghinvite.
//!
//! Restate is the authority for every durable state change —
//! [`admission`] and [`request_lifecycle`] for invitation requests,
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

pub mod admission;
pub mod availability;
pub mod delivery;
pub mod error;
pub mod github_invitation;
pub mod installation;
pub mod projection;
pub mod reconcile;
pub mod repository_access;
pub mod request_lifecycle;
pub mod settlement;
pub mod state;
pub mod throttle;

#[cfg(test)]
pub(crate) mod test_support;

pub use error::{HandlerError, Result};
pub use state::{AppState, WorkflowStorage};

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
/// **Production:** always pass `Some` — set `RESTATE_IDENTITY_KEY` via
/// `wrangler secret put RESTATE_IDENTITY_KEY --config wrangler/restate-svc.toml`.
pub fn build_endpoint(
    state: AppState,
    projection: std::sync::Arc<dyn ghinvite_core::storage::projection::ProjectionStorage>,
    identity_key: Option<&str>,
) -> std::result::Result<Endpoint, String> {
    let builder = Endpoint::builder()
        .bind(installation::Installation {
            state: state.clone(),
        })
        .bind(github_invitation::GithubInvitation {
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
    let builder = admission::bind(builder);
    let builder = projection::bind(builder, projection);
    let builder = request_lifecycle::bind(builder);
    let mut builder = delivery::bind(builder, state);
    if let Some(key) = identity_key {
        builder = builder
            .identity_key(key)
            .map_err(|e| format!("RESTATE_IDENTITY_KEY is invalid: {e}"))?;
    }
    Ok(builder.build())
}
