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
pub mod share_link;
pub mod state;

#[cfg(any(test))]
pub(crate) mod test_support;

// Re-exports filled in as each module gains its public types:
// pub use error::{HandlerError, Result};
// pub use state::AppState;
