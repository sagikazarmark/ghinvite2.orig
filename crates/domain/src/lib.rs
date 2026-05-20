//! Pure domain types and state derivations. No I/O.

pub mod account;
pub mod github_invitation;
pub mod ids;
pub mod invitation_request;
pub mod permission;
pub mod repository_identity;
pub mod share_link;
pub mod slug;
pub mod user;

// Re-exports filled in as each module gains its public types:
pub use account::{Account, AccountType, SelectedRepos};
pub use github_invitation::{GithubInvitation, InvitationState};
pub use ids::{AuditEventId, GithubInvitationId, RequestId, ShareLinkId};
pub use invitation_request::{InvitationRequest, RequestState};
pub use permission::Permission;
pub use repository_identity::{RepositoryIdentity, RepositoryIdentityError};
pub use share_link::{ShareLink, ShareLinkRepo};
pub use slug::Slug;
pub use user::User;
