//! ghinvite's installation token scopes ("profiles"), built once.
//!
//! Each profile is the exact permission set a token is created with, and the
//! cache key next to the installation (`octotoken`). Read profiles grant only
//! `read`, which replaces gh-toolkit's compile-time `ReadOnlyBroker` check
//! with the tests below; the endpoint ↔ profile assignment test classifies
//! every [`Endpoint`](crate::endpoints::Endpoint).
//!
//! Pass them by reference: `app.read(installation, &REPO_READ, …)`.

use octoauth::{Access, OrganizationPermission, Permissions, RepositoryPermission, TokenScope};
use std::sync::LazyLock;

/// Workflows, repository app: reads (administration, metadata).
pub static REPO_READ: LazyLock<TokenScope> = LazyLock::new(|| {
    scope(
        Permissions::builder()
            .grant(RepositoryPermission::Administration, Access::Read)
            .grant(RepositoryPermission::Metadata, Access::Read),
    )
});

/// Workflows, repository app: collaborator invitations and their reads.
pub static REPO_INVITE: LazyLock<TokenScope> = LazyLock::new(|| {
    scope(
        Permissions::builder()
            .grant(RepositoryPermission::Administration, Access::Write)
            .grant(RepositoryPermission::Metadata, Access::Read),
    )
});

/// Workflows, organization app: reads (members, metadata).
pub static ORG_READ: LazyLock<TokenScope> = LazyLock::new(|| {
    scope(
        Permissions::builder()
            .grant(OrganizationPermission::Members, Access::Read)
            .grant(RepositoryPermission::Metadata, Access::Read),
    )
});

/// Workflows, organization app: organization invitations, team memberships
/// and removals, and their reads.
pub static ORG_INVITE: LazyLock<TokenScope> = LazyLock::new(|| {
    scope(
        Permissions::builder()
            .grant(OrganizationPermission::Members, Access::Write)
            .grant(RepositoryPermission::Metadata, Access::Read),
    )
});

/// Web console, repository app: repository metadata only.
pub static CONSOLE_REPO_READ: LazyLock<TokenScope> = LazyLock::new(|| {
    scope(Permissions::builder().grant(RepositoryPermission::Metadata, Access::Read))
});

/// Web console, organization app: members only.
pub static CONSOLE_ORG_READ: LazyLock<TokenScope> = LazyLock::new(|| {
    scope(Permissions::builder().grant(OrganizationPermission::Members, Access::Read))
});

fn scope(permissions: octoauth::PermissionsBuilder) -> TokenScope {
    TokenScope::new(permissions.build().expect("static permission profile"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        endpoints::{Access as Required, Endpoint},
        rest::CredentialKind,
    };

    fn json(scope: &TokenScope) -> String {
        serde_json::to_string(scope.permissions()).unwrap()
    }

    #[test]
    fn profiles_request_exactly_these_permissions() {
        assert_eq!(
            json(&REPO_READ),
            r#"{"administration":"read","metadata":"read"}"#
        );
        assert_eq!(
            json(&REPO_INVITE),
            r#"{"administration":"write","metadata":"read"}"#
        );
        assert_eq!(json(&ORG_READ), r#"{"members":"read","metadata":"read"}"#);
        assert_eq!(
            json(&ORG_INVITE),
            r#"{"members":"write","metadata":"read"}"#
        );
        assert_eq!(json(&CONSOLE_REPO_READ), r#"{"metadata":"read"}"#);
        assert_eq!(json(&CONSOLE_ORG_READ), r#"{"members":"read"}"#);
    }

    #[test]
    fn read_profiles_grant_only_read_access() {
        for profile in [&REPO_READ, &ORG_READ, &CONSOLE_REPO_READ, &CONSOLE_ORG_READ] {
            assert!(profile.permissions().is_read_only(), "{profile:?}");
        }
        for profile in [&REPO_INVITE, &ORG_INVITE] {
            assert!(!profile.permissions().is_read_only(), "{profile:?}");
        }
        // Distinct scopes are distinct token-cache keys.
        let all = [
            &*REPO_READ,
            &*REPO_INVITE,
            &*ORG_READ,
            &*ORG_INVITE,
            &*CONSOLE_REPO_READ,
            &*CONSOLE_ORG_READ,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    fn grants(profile: &TokenScope, endpoint: Endpoint) -> bool {
        let Some((permission, required)) = endpoint.meta().permission else {
            return true;
        };
        let granted = profile.permissions().get(permission);
        match required {
            Required::Read => granted.is_some(),
            Required::Write => matches!(granted, Some(Access::Write | Access::Admin)),
            Required::Admin => granted == Some(Access::Admin),
        }
    }

    #[test]
    fn numeric_actor_lookup_is_read_only_under_both_installed_app_profiles() {
        let meta = Endpoint::GetUserById.meta();
        assert!(meta.credential.contains(&CredentialKind::Installation));
        assert!(
            meta.permission
                .is_none_or(|(_, access)| access == Required::Read)
        );
        for profile in [&REPO_READ, &ORG_READ] {
            assert!(grants(profile, Endpoint::GetUserById));
        }
    }

    /// AC-BRK-3, moved from `ghinvite-workflows/src/github_profiles.rs`:
    /// every endpoint is consciously assigned a workflow profile (the match is
    /// exhaustive inside this crate), and the profile grants what its
    /// metadata requires. App-JWT endpoints borrow no installation profile.
    #[test]
    fn each_endpoint_has_an_explicit_app_profile_assignment() {
        for &endpoint in Endpoint::all() {
            let profile: Option<&TokenScope> = match endpoint {
                Endpoint::GetRepository
                | Endpoint::CollaboratorPermission
                | Endpoint::ListCollaborators
                | Endpoint::ListRepoInvitations
                | Endpoint::GetUserByLogin
                | Endpoint::GetUserById
                | Endpoint::ListInstallationRepos => Some(&REPO_READ),
                Endpoint::PutCollaborator
                | Endpoint::UpdateRepoInvitation
                | Endpoint::DeleteRepoInvitation
                | Endpoint::RemoveCollaborator => Some(&REPO_INVITE),
                Endpoint::GetOrgMembership
                | Endpoint::GetOrg
                | Endpoint::ListOrgInvitations
                | Endpoint::ListFailedOrgInvitations
                | Endpoint::ListOrgInvitationTeams
                | Endpoint::ListOrgTeams
                | Endpoint::GetTeam
                | Endpoint::GetTeamMembership => Some(&ORG_READ),
                Endpoint::CreateOrgInvitation
                | Endpoint::CancelOrgInvitation
                | Endpoint::RemoveOrgMember
                | Endpoint::PutTeamMembership
                | Endpoint::RemoveTeamMembership => Some(&ORG_INVITE),
                Endpoint::GetInstallation | Endpoint::ListInstallationRequests => None,
            };
            let meta = endpoint.meta();
            match profile {
                Some(profile) => {
                    assert!(
                        meta.credential.contains(&CredentialKind::Installation),
                        "{endpoint:?}"
                    );
                    assert!(
                        grants(profile, endpoint),
                        "{endpoint:?} exceeds {profile:?}"
                    );
                }
                None => {
                    assert_eq!(meta.credential, &[CredentialKind::AppJwt], "{endpoint:?}");
                    assert!(meta.permission.is_none(), "{endpoint:?}");
                }
            }
        }
    }
}
