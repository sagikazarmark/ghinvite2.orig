//! Restate handler services for ghinvite. Five services own every durable
//! state change: [`installation::Installation`] (Virtual Object),
//! [`share_link::ShareLink`] (Virtual Object), [`invitation_request::InvitationRequest`]
//! (Workflow), [`github_invitation::GithubInvitation`] (Virtual Object),
//! [`reconcile::Reconcile`] (Service).
//!
//! Each handler delegates to a pure-async function that takes [`state::AppState`]
//! and returns [`error::HandlerError`]. Unit tests exercise the pure functions
//! without a Restate runtime; full workflow integration tests against the local
//! `restate-server` (compose.yaml) are deferred to Plan 8.

pub mod audit;
pub mod error;
pub mod github_invitation;
pub mod installation;
pub mod invitation_request;
pub mod reconcile;
pub(crate) mod restate_payload;
pub mod share_link;
pub mod state;

#[cfg(test)]
pub(crate) mod test_support;

// Re-exports filled in as each module gains its public types:
pub use error::{HandlerError, Result};
pub use state::AppState;

use restate_sdk::endpoint::Endpoint;

/// Build a fully-bound Restate endpoint with all five ghinvite services.
/// Plan 7's Workers `#[event(fetch)]` calls this once per cold start.
pub fn build_endpoint(state: AppState) -> Endpoint {
    use github_invitation::GithubInvitation as _;
    use installation::Installation as _;
    use invitation_request::InvitationRequest as _;
    use reconcile::Reconcile as _;
    use share_link::ShareLink as _;

    Endpoint::builder()
        .bind(installation::InstallationImpl { state: state.clone() }.serve())
        .bind(share_link::ShareLinkImpl { state: state.clone() }.serve())
        .bind(invitation_request::InvitationRequestImpl { state: state.clone() }.serve())
        .bind(github_invitation::GithubInvitationImpl { state: state.clone() }.serve())
        .bind(reconcile::ReconcileImpl { state }.serve())
        .build()
}
