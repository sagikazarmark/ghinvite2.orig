//! Numeric GitHub identities.
//!
//! App and installation IDs are octoauth's, so a credential's identity and
//! the token it came from share one type. octoauth has no user, repository,
//! organization or team IDs; those stay here.

pub use octoauth::{AppId, InstallationId};

macro_rules! id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(u64);
        impl $name {
            /// Wrap a numeric GitHub identifier.
            pub const fn new(value: u64) -> Self {
                Self(value)
            }
            /// The numeric identifier.
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}
id!(UserId, "Numeric GitHub user identity.");
id!(RepoId, "Numeric GitHub repository identity.");
id!(OrgId, "Numeric GitHub organization identity.");
id!(TeamId, "Numeric GitHub team identity.");
