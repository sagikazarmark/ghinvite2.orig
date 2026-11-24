//! The core of ghinvite: pure domain types and state derivations, audit event
//! payloads, and the [`storage::Storage`] port. No I/O of its own —
//! implementations of the storage port live in `ghinvite-storage-sqlx`
//! (native) and `ghinvite-storage-d1` (Cloudflare Workers).

pub mod access_grant;
pub mod account;
pub mod admission;
pub mod audit;
pub mod budget;
pub mod bulk;
pub mod bulk_cancel;
pub mod bulk_commands;
pub mod bulk_preview;
pub mod clock;
pub mod delivery;
pub mod delivery_audit;
pub mod delivery_control;
pub mod email_delivery;
pub mod email_domain;
pub mod github_invitation;
pub mod ids;
pub mod installation_audit;
pub mod invitation_link;
pub mod invitation_request;
pub mod link_lifecycle;
pub mod link_secret;
pub mod org_delivery;
pub mod org_eligibility;
pub mod org_observation;
pub mod org_webhook;
pub mod permission;
pub mod purpose;
pub mod repository_identity;
pub mod request_completion;
pub mod request_control;
pub mod request_lifecycle;
pub mod revocation;
pub mod storage;
pub mod sweep_health;
pub mod targets;
pub mod user;

// Re-exports filled in as each module gains its public types:
pub use account::{Account, AccountType, SelectedRepos};
#[cfg(any(test, feature = "test-suite"))]
pub use github_invitation::GithubInvitation;
pub use github_invitation::InvitationState;
pub use ids::{AuditEventId, GithubInvitationId, InvitationLinkId, RequestId};
pub use invitation_link::{
    Description, DescriptionError, Inactive, InternalNote, InternalNoteTooLong, InvitationLink,
    InvitationLinkRepo, RepositoryScope, RepositoryScopeError, RotateSecret, SecretRotated,
};
pub use invitation_request::{InvitationRequest, RequestCancellationReason, RequestState};
pub use permission::Permission;
pub use repository_identity::{RepositoryIdentity, RepositoryIdentityError};
pub use user::User;

pub use link_secret::{LinkSecretState, SecretHash};

pub mod reissue;
pub mod reissue_projection;

mod personal_link;
