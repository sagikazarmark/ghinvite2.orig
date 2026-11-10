//! GitHub repository roles, independent of application access policy.

/// An effective repository role reported by GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepoRole {
    /// Read repository contents.
    Read,
    /// Triage issues and pull requests.
    Triage,
    /// Write repository contents.
    Write,
    /// Maintain repository settings.
    Maintain,
    /// Administer the repository.
    Admin,
    /// No repository access.
    None,
    /// An organization-defined role whose precedence is unknown.
    Custom(String),
}

impl RepoRole {
    /// Parse modern or legacy wire names, preserving custom roles.
    pub fn parse(value: &str) -> Self {
        match value {
            "read" | "pull" => Self::Read,
            "triage" => Self::Triage,
            "write" | "push" => Self::Write,
            "maintain" => Self::Maintain,
            "admin" => Self::Admin,
            "none" => Self::None,
            other => Self::Custom(other.to_owned()),
        }
    }

    /// Built-in precedence. Custom roles cannot be compared safely.
    pub const fn rank(&self) -> Option<u8> {
        match self {
            Self::None => Some(0),
            Self::Read => Some(1),
            Self::Triage => Some(2),
            Self::Write => Some(3),
            Self::Maintain => Some(4),
            Self::Admin => Some(5),
            Self::Custom(_) => None,
        }
    }

    /// Whether this is exactly the requested built-in grant.
    pub fn matches(&self, requested: &Self) -> bool {
        self == requested && self.rank().is_some_and(|rank| rank > 0)
    }

    /// Whether GitHub reports any access, including a custom role.
    pub fn has_access(&self) -> bool {
        *self != Self::None
    }
}

impl std::fmt::Display for RepoRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Read => "pull",
            Self::Triage => "triage",
            Self::Write => "push",
            Self::Maintain => "maintain",
            Self::Admin => "admin",
            Self::None => "none",
            Self::Custom(name) => name,
        })
    }
}

/// Built-in organization membership role. Unknown wire values fail decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OrgRole {
    /// Active organization membership without organization-owner authority.
    Member,
    /// Organization-owner role, subject to a separately verified active membership state.
    Admin,
}

impl OrgRole {
    /// Order known organization roles for comparison; this does not establish membership or authority by itself.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Member => 1,
            Self::Admin => 2,
        }
    }
    /// GitHub's canonical wire value for this known organization role.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
        }
    }
}

/// Explicit team membership role. Never default a maintainer to a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TeamRole {
    /// Ordinary team membership without team-maintainer privileges.
    Member,
    /// Team-maintainer role; not organization-owner or repository-administrator authority.
    Maintainer,
}

impl TeamRole {
    /// Order known team roles for comparison without treating unknown roles as sufficient.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Member => 1,
            Self::Maintainer => 2,
        }
    }
    /// GitHub's canonical wire value for this known team role.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Maintainer => "maintainer",
        }
    }
}

/// A pending membership is not evidence of accepted access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MembershipState {
    /// GitHub explicitly confirms that membership is active.
    Active,
    /// Membership is pending and must not be presented as active access.
    Pending,
}

#[cfg(test)]
mod tests {
    use super::RepoRole;

    #[test]
    fn builtin_precedence_and_legacy_names() {
        for (rank, name) in ["none", "read", "triage", "write", "maintain", "admin"]
            .iter()
            .enumerate()
        {
            assert_eq!(RepoRole::parse(name).rank(), Some(rank as u8));
        }
        assert_eq!(RepoRole::parse("pull"), RepoRole::Read);
        assert_eq!(RepoRole::parse("push"), RepoRole::Write);
    }

    #[test]
    fn custom_roles_do_not_imply_precedence() {
        let role = RepoRole::parse("security-auditor");
        assert_eq!(role.rank(), None);
        assert!(role.has_access());
        assert!(!role.matches(&role));
        assert!(!RepoRole::None.has_access());
    }
}
