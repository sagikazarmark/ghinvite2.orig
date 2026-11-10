//! Advisory organization facts. Missing token-visible fields stay unknown;
//! cached warnings never authorize a write or replace fresh delivery checks.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CACHE_SECONDS: i64 = 3600;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BasePermission {
    None,
    Read,
    Write,
    Admin,
}
impl BasePermission {
    pub fn covers(self, grant: crate::targets::RepoRole) -> bool {
        let rank = match self {
            Self::None => 0,
            Self::Read => 1,
            Self::Write => 3,
            Self::Admin => 5,
        };
        rank >= grant.rank()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Seats {
    pub capacity: u64,
    pub used: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    pub account_id: u64,
    pub installation_id: u64,
    /// Beginning of the complete identity/read observation, never cache-write time.
    pub observed_at: DateTime<Utc>,
    pub seats: Option<Seats>,
    pub two_factor_required: Option<bool>,
    pub base_permission: Option<BasePermission>,
    /// Current documented minimal app APIs supply no reliable EMU discriminator.
    pub managed_enterprise: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    NoSeat,
    TwoFactorRequired,
    BasePermissionAlreadySufficient { repo_id: u64 },
    ManagedEnterprise,
}
impl Facts {
    pub fn fresh(&self, account_id: u64, installation_id: u64, now: DateTime<Utc>) -> bool {
        self.account_id == account_id
            && self.installation_id == installation_id
            && now >= self.observed_at
            && now < self.observed_at + chrono::Duration::seconds(CACHE_SECONDS)
    }
    pub fn warnings(&self, targets: &crate::targets::Targets) -> Vec<Warning> {
        let mut warnings = Vec::new();
        if self
            .seats
            .is_some_and(|s| s.capacity > 0 && s.used >= s.capacity)
        {
            warnings.push(Warning::NoSeat);
        }
        if self.two_factor_required == Some(true) {
            warnings.push(Warning::TwoFactorRequired);
        }
        if self.managed_enterprise == Some(true) {
            warnings.push(Warning::ManagedEnterprise);
        }
        if let Some(base) = self.base_permission {
            warnings.extend(
                targets
                    .repos
                    .iter()
                    .filter(|r| base.covers(r.grant))
                    .map(|r| Warning::BasePermissionAlreadySufficient { repo_id: r.repo_id }),
            );
        }
        warnings
    }
}
/// Authenticated account-level capacity hint, deliberately distinct from a
/// requester-bound member-added wake. A removed member need not be a requester.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberRemoved {
    pub account_id: u64,
    pub installation_id: u64,
    pub removed_user_id: u64,
    pub payload_sha256: [u8; 32],
}
impl MemberRemoved {
    pub fn validate(&self) -> Result<(), &'static str> {
        if [self.account_id, self.installation_id, self.removed_user_id]
            .into_iter()
            .all(|id| id > 0 && id <= crate::storage::integer::MAX_EXACT)
        {
            Ok(())
        } else {
            Err("invalid organization capacity hint")
        }
    }
    pub fn event_key(&self) -> String {
        self.payload_sha256
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstallationHint {
    pub account_id: u64,
    pub installation_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_hour_boundary_future_and_reinstall_are_not_cached_facts() {
        let at = DateTime::from_timestamp(10000, 0).unwrap();
        let f = Facts {
            account_id: 100,
            installation_id: 2,
            observed_at: at,
            seats: None,
            two_factor_required: None,
            base_permission: None,
            managed_enterprise: None,
        };
        assert!(f.fresh(100, 2, at));
        assert!(f.fresh(100, 2, at + chrono::Duration::seconds(3599)));
        assert!(!f.fresh(100, 2, at + chrono::Duration::seconds(3600)));
        assert!(!f.fresh(100, 2, at - chrono::Duration::seconds(1)));
        assert!(!f.fresh(100, 3, at));
        assert!(!f.fresh(101, 2, at));
        assert!(f.warnings(&crate::targets::Targets::default()).is_empty());
    }
    #[test]
    fn warnings_require_positive_facts_and_preserve_each_repository_grant() {
        use crate::targets::{RepoRole, RepoTarget, Targets};
        let targets = Targets {
            repos: vec![
                RepoTarget {
                    repo_id: 1,
                    repo_full_name: "acme/read".into(),
                    grant: RepoRole::Read,
                },
                RepoTarget {
                    repo_id: 2,
                    repo_full_name: "acme/admin".into(),
                    grant: RepoRole::Admin,
                },
            ],
            ..Default::default()
        };
        let facts = Facts {
            account_id: 100,
            installation_id: 2,
            observed_at: Utc::now(),
            seats: Some(Seats {
                capacity: 10,
                used: 11,
            }),
            two_factor_required: Some(true),
            base_permission: Some(BasePermission::Write),
            managed_enterprise: None,
        };
        assert_eq!(
            facts.warnings(&targets),
            vec![
                Warning::NoSeat,
                Warning::TwoFactorRequired,
                Warning::BasePermissionAlreadySufficient { repo_id: 1 }
            ]
        );
        assert!(!BasePermission::None.covers(crate::targets::RepoRole::Read));
    }
}
