//! The core of ghinvite: pure domain types and state derivations, audit event
//! payloads, and the [`storage::Storage`] port. No I/O of its own —
//! implementations of the storage port live in `ghinvite-storage-sqlx`
//! (native) and `ghinvite-storage-d1` (Cloudflare Workers).

pub mod account;
pub mod admission;
pub mod audit;
pub mod delivery;
pub mod github_invitation;
pub mod ids;
pub mod invitation_link;
pub mod invitation_request;
pub mod permission;
pub mod repository_identity;
pub mod request_lifecycle;
pub mod storage;
pub mod user;

// Re-exports filled in as each module gains its public types:
pub use account::{Account, AccountType, SelectedRepos};
pub use github_invitation::{GithubInvitation, InvitationState};
pub use ids::{AuditEventId, GithubInvitationId, InvitationLinkId, RequestId};
pub use invitation_link::{
    Description, DescriptionError, Inactive, InternalNote, InternalNoteTooLong, InvitationLink,
    InvitationLinkRepo, RepositoryScope, RepositoryScopeError,
};
pub use invitation_request::{InvitationRequest, RequestState};
pub use permission::Permission;
pub use repository_identity::{RepositoryIdentity, RepositoryIdentityError};
pub use user::User;
