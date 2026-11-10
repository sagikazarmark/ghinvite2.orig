//! Target grants and observed roles are distinct: custom roles are never grants.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RepoRole {
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
}
impl RepoRole {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Triage => 2,
            Self::Write => 3,
            Self::Maintain => 4,
            Self::Admin => 5,
        }
    }
}
impl From<crate::Permission> for RepoRole {
    fn from(value: crate::Permission) -> Self {
        match value {
            crate::Permission::Pull => Self::Read,
            crate::Permission::Triage => Self::Triage,
            crate::Permission::Push => Self::Write,
            crate::Permission::Maintain => Self::Maintain,
            crate::Permission::Admin => Self::Admin,
        }
    }
}
impl From<RepoRole> for crate::Permission {
    fn from(value: RepoRole) -> Self {
        match value {
            RepoRole::Read => Self::Pull,
            RepoRole::Triage => Self::Triage,
            RepoRole::Write => Self::Push,
            RepoRole::Maintain => Self::Maintain,
            RepoRole::Admin => Self::Admin,
        }
    }
}

/// Bounded role label for an admin's display; never an arbitrary upstream error.
#[derive(Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct CustomRoleName(String);
impl CustomRoleName {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() || value.len() > 100 || value.chars().any(char::is_control) {
            return Err("invalid custom role name");
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for CustomRoleName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CustomRoleName([redacted])")
    }
}
impl<'de> Deserialize<'de> for CustomRoleName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveRole {
    None,
    BaseRole,
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
    Custom(CustomRoleName),
}
impl EffectiveRole {
    pub const fn rank(&self) -> Option<u8> {
        match self {
            Self::None => Some(0),
            Self::Read => Some(1),
            Self::Triage => Some(2),
            Self::Write => Some(3),
            Self::Maintain => Some(4),
            Self::Admin => Some(5),
            Self::BaseRole | Self::Custom(_) => None,
        }
    }
    pub fn satisfies(&self, grant: RepoRole) -> bool {
        self.rank().is_some_and(|r| r >= grant.rank())
    }
}
impl From<RepoRole> for EffectiveRole {
    fn from(role: RepoRole) -> Self {
        match role {
            RepoRole::Read => Self::Read,
            RepoRole::Triage => Self::Triage,
            RepoRole::Write => Self::Write,
            RepoRole::Maintain => Self::Maintain,
            RepoRole::Admin => Self::Admin,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoTarget {
    pub repo_id: u64,
    pub repo_full_name: String,
    pub grant: RepoRole,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberGrant {
    Member,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationTarget {
    pub org_id: u64,
    pub org_login: String,
    pub grant: MemberGrant,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TeamTarget {
    pub team_id: u64,
    pub team_slug: String,
    pub grant: MemberGrant,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Targets {
    pub organization: Option<OrganizationTarget>,
    pub teams: Vec<TeamTarget>,
    pub repos: Vec<RepoTarget>,
}
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TargetError {
    #[error("targets are empty")]
    Empty,
    #[error("too many targets")]
    TooMany,
    #[error("teams require an organization target")]
    TeamsRequireOrganization,
    #[error("invalid repository target")]
    InvalidRepository,
    #[error("invalid team target")]
    InvalidTeam,
    #[error("invalid organization target")]
    InvalidOrganization,
    #[error("targets must belong to one account")]
    DifferentAccounts,
}
impl Targets {
    pub fn normalized(mut self) -> Result<Self, TargetError> {
        self.validate()?;
        self.repos.sort_by_key(|r| r.repo_id);
        self.teams.sort_by_key(|t| t.team_id);
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), TargetError> {
        use std::collections::BTreeSet;
        let valid = |id: u64| id > 0 && id <= ((1u64 << 53) - 1);
        if self.organization.is_none() && self.repos.is_empty() {
            return Err(TargetError::Empty);
        }
        if self.teams.len() > 100 || self.repos.len() > 100 {
            return Err(TargetError::TooMany);
        }
        if !self.teams.is_empty() && self.organization.is_none() {
            return Err(TargetError::TeamsRequireOrganization);
        }
        let mut repos = BTreeSet::new();
        let mut teams = BTreeSet::new();
        let mut owner = self
            .organization
            .as_ref()
            .map(|o| o.org_login.to_ascii_lowercase());
        for r in &self.repos {
            if !valid(r.repo_id)
                || !repos.insert(r.repo_id)
                || r.repo_full_name.len() > 256
                || r.repo_full_name.chars().any(char::is_control)
                || crate::RepositoryIdentity::parse(&r.repo_full_name).is_err()
            {
                return Err(TargetError::InvalidRepository);
            }
            let identity = crate::RepositoryIdentity::parse(&r.repo_full_name)
                .map_err(|_| TargetError::InvalidRepository)?;
            let next = identity.owner().to_ascii_lowercase();
            if owner.as_ref().is_some_and(|expected| expected != &next) {
                return Err(TargetError::DifferentAccounts);
            }
            owner = Some(next);
        }
        for t in &self.teams {
            if !valid(t.team_id)
                || !teams.insert(t.team_id)
                || t.team_slug.is_empty()
                || t.team_slug.len() > 100
                || t.team_slug.chars().any(char::is_control)
            {
                return Err(TargetError::InvalidTeam);
            }
        }
        if let Some(o) = &self.organization
            && (!valid(o.org_id)
                || o.org_login.is_empty()
                || o.org_login.len() > 100
                || o.org_login.chars().any(char::is_control))
        {
            return Err(TargetError::InvalidOrganization);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_roles_cannot_be_ranked_or_granted() {
        let r = EffectiveRole::Custom(CustomRoleName::new("security-reviewer").unwrap());
        assert_eq!(r.rank(), None);
        assert!(!r.satisfies(RepoRole::Read));
        assert!(serde_json::from_str::<RepoRole>("\"security-reviewer\"").is_err());
        assert!(serde_json::from_str::<CustomRoleName>("\"bad\\nrole\"").is_err());
    }
    #[test]
    fn heterogeneous_grants_remain_distinct() {
        let t = Targets {
            repos: vec![
                RepoTarget {
                    repo_id: 1,
                    repo_full_name: "a/one".into(),
                    grant: RepoRole::Read,
                },
                RepoTarget {
                    repo_id: 2,
                    repo_full_name: "a/two".into(),
                    grant: RepoRole::Admin,
                },
            ],
            ..Targets::default()
        };
        t.validate().unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Targets>(&json).unwrap(), t);
    }
}
