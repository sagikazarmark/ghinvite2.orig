//! Restate handler services for ghinvite. Five services own every durable
//! state change: [`installation::Installation`] (Virtual Object),
//! [`invitation_link::InvitationLink`] (Virtual Object), [`invitation_request::InvitationRequest`]
//! (Workflow), [`github_invitation::GithubInvitation`] (Virtual Object),
//! [`reconcile::Reconcile`] (Service).
//!
//! Each handler delegates to a pure-async function that takes [`state::AppState`]
//! and returns [`error::HandlerError`]. Unit tests exercise the pure functions
//! without a Restate runtime; full workflow integration tests against the local
//! `restate-server` (compose.yaml) are deferred to Plan 8.

pub mod admission_v1;
pub mod audit;
pub mod error;
pub mod github_invitation;
pub mod installation;
pub mod invitation_context;
pub mod invitation_link;
pub mod invitation_request;
pub mod projection_v1;
pub mod reconcile;
pub mod state;

#[cfg(test)]
pub(crate) mod test_support;

// Re-exports filled in as each module gains its public types:
pub use error::{HandlerError, Result};
pub use state::AppState;

use restate_sdk::endpoint::Endpoint;

/// Build a fully-bound Restate endpoint with all five ghinvite services.
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
        .bind(reconcile::ReconcileImpl { state }.serve());

    if let Some(key) = identity_key {
        builder = builder
            .identity_key(key)
            .map_err(|e| format!("RESTATE_IDENTITY_KEY is invalid: {e}"))?;
    }

    Ok(builder.build())
}
